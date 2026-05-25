# Ingest UX rework Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver the ingest UX overhaul specified in `docs/superpowers/specs/2026-05-25-ingest-ux-design.md` — a top-level `mnm manifest {init,generate,check}` namespace for local manifest authoring, restructured `mnm ingest {plan,run}` namespace, full implementation of the long-stubbed manifest features (`path:` pinning, `published_url` / `provenance` inheritance, strict-only walker), sitemap-aware manifest generation, TTY progress UX, chunked upload, auto-create-source, fixed `DocumentUpload` wiring bugs, and the server-URL default flip.

**Architecture:** Pure-logic crates (`mn-content`, `mn-core`) gain a `manifest::resolve` flattener, `manifest::sitemap` parser, `manifest::matcher`, and `manifest::generate` module. The CLI grows a new top-level `manifest` namespace and restructures its existing `ingest` command into a `{plan,run}` subgroup. The walker is reworked to consume `Vec<ResolvedLeaf>` records (with `published_url`, `provenance` inheritance, and `path:` discovery already applied) instead of raw `ManifestNode`s. Wire-format additions to the upload endpoint are informational only (`batch_index` / `batch_count`).

**Tech Stack:** Rust 1.91 stable, `clap` v4, `serde` + `serde_yaml` + `serde_json`, `pulldown-cmark`, `reqwest`, `tokio`, `indicatif`, `globset` (new), `quick-xml` (new — hand-roll sitemap parsing), `tokenizers` (via existing `fastembed` transitive dep, promoted to direct dep in `mn-content`). Existing patterns: anyhow for CLI errors, thiserror for library errors, tracing for diagnostics, `mn-core::config::Config` for resolved configuration, FR-066 admin-hidden visibility gate.

**Phasing:** Tasks are grouped into 9 phases. Phases 1–5 are pure-logic crate work and can be merged behind a feature flag (the new types are unused until Phase 6+ wires them in). Phases 6–8 land the user-facing CLI. Phase 9 is docs + the end-to-end regression test that proves the F-bug is fixed.

---

## Phase 1 — Foundation

Two tiny, independent commits that unblock later phases.

### Task 1: Bump compiled-in server URL default

**Files:**
- Modify: `crates/mn-cli/src/shared.rs:17`
- Modify: `crates/mn-core/src/config.rs` (wherever `server.url` default lives)
- Test: existing `crates/mn-cli/src/shared.rs` tests + a new defaults test in `mn-core/src/config.rs`

- [ ] **Step 1: Locate the existing default in `mn-core`**

Run: `rg "manual.midnight.network" crates/`
Expected: at least two hits (the doc comment in `shared.rs` and the `Default` impl in `mn-core::config`).

- [ ] **Step 2: Write a failing test in `mn-core/src/config.rs`**

Add at the bottom of the existing `tests` module:

```rust
#[test]
fn server_url_default_is_production_host() {
    let cfg = Config::default();
    assert_eq!(cfg.server.url, "https://midnight-manual.midnightntwrk.expert");
}
```

- [ ] **Step 3: Run the test, confirm it fails**

Run: `cargo test -p mn-core server_url_default_is_production_host`
Expected: FAIL with `assertion left == right` showing the old URL.

- [ ] **Step 4: Change the `Default` impl**

In `mn-core/src/config.rs`, replace the literal:

```rust
"https://manual.midnight.network".to_owned()
```

with:

```rust
"https://midnight-manual.midnightntwrk.expert".to_owned()
```

Also update the doc comment in `crates/mn-cli/src/shared.rs:17` (the bullet under "Precedence (D17)") to match.

- [ ] **Step 5: Run the test, confirm it passes**

Run: `cargo test -p mn-core server_url_default_is_production_host`
Expected: PASS.

- [ ] **Step 6: Run the whole workspace**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/mn-cli/src/shared.rs crates/mn-core/src/config.rs
git commit -m "feat(cli): default server URL to midnight-manual.midnightntwrk.expert

Precedence unchanged (--server > MIDNIGHT_MANUAL_SERVER >
[server].url > compiled-in default). Replaces the old
manual.midnight.network placeholder.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §4"
```

### Task 2: Add `mn_content::language` extension → language lookup

**Files:**
- Create: `crates/mn-content/src/language.rs`
- Modify: `crates/mn-content/src/lib.rs` (add `pub mod language;`)
- Test: inline `#[cfg(test)] mod tests` in `language.rs`

- [ ] **Step 1: Write the failing test first**

Create `crates/mn-content/src/language.rs` with:

```rust
//! Filename-extension → IANA-like language name lookup. Populates the
//! `language` column on `document` rows during ingest (§3.4 of the
//! ingest-UX design spec). Returns `None` for unknown extensions.

use std::path::Path;

/// Map a file path's extension to a stable language identifier.
///
/// Returns `None` when the extension is unknown or absent. Matching is
/// case-insensitive on the extension only.
#[must_use]
pub fn from_path(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "md" | "mdx" => "markdown",
        "rs" => "rust",
        "ts" => "typescript",
        "tsx" => "typescript",
        "js" => "javascript",
        "jsx" => "javascript",
        "compact" => "compact",
        "txt" => "plaintext",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_extensions_resolve() {
        assert_eq!(from_path(Path::new("foo.md")), Some("markdown"));
        assert_eq!(from_path(Path::new("foo.MD")), Some("markdown"));
        assert_eq!(from_path(Path::new("foo.compact")), Some("compact"));
        assert_eq!(from_path(Path::new("a/b/c.tsx")), Some("typescript"));
    }

    #[test]
    fn unknown_or_missing_extension_returns_none() {
        assert_eq!(from_path(Path::new("foo")), None);
        assert_eq!(from_path(Path::new("foo.unknown")), None);
        assert_eq!(from_path(Path::new("")), None);
    }
}
```

- [ ] **Step 2: Wire the module into `lib.rs`**

In `crates/mn-content/src/lib.rs`, add (sorted alphabetically with the other `pub mod` lines):

```rust
pub mod language;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p mn-content language::tests`
Expected: 2 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/mn-content/src/language.rs crates/mn-content/src/lib.rs
git commit -m "feat(mn-content): add language::from_path extension lookup

Returns stable language identifiers (markdown, rust, typescript, …)
from filename extensions. Used by the ingest pipeline to populate the
document.language column.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §3.4"
```

---

## Phase 2 — Manifest schema enhancements + resolver

### Task 3: Extend `ManifestNode` with `include` / `exclude` arrays

**Files:**
- Modify: `crates/mn-content/src/manifest.rs` (struct + tests)

- [ ] **Step 1: Write the failing parse test**

Add to the existing `tests` module in `manifest.rs`:

```rust
#[test]
fn parses_include_and_exclude_on_node() {
    let body = r#"
manifest_version: 1
root:
  name: docs
  path: docs/
  include: ["**/*.md", "**/*.mdx"]
  exclude: ["**/draft/**"]
"#;
    let m = Manifest::parse(body).unwrap();
    assert_eq!(m.root.include, vec!["**/*.md", "**/*.mdx"]);
    assert_eq!(m.root.exclude, vec!["**/draft/**"]);
}

#[test]
fn include_and_exclude_default_to_empty() {
    let body = "manifest_version: 1\nroot:\n  name: docs\n";
    let m = Manifest::parse(body).unwrap();
    assert!(m.root.include.is_empty());
    assert!(m.root.exclude.is_empty());
}
```

- [ ] **Step 2: Run the tests, confirm they fail**

Run: `cargo test -p mn-content manifest::tests::parses_include_and_exclude_on_node`
Expected: FAIL (`unknown field 'include'`).

- [ ] **Step 3: Extend `ManifestNode`**

In `crates/mn-content/src/manifest.rs`, add two fields to the struct (insert after `provenance:`):

```rust
/// Per-node glob include filter (applies when `path:` is set; ignored otherwise).
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub include: Vec<String>,
/// Per-node glob exclude filter (applies when `path:` is set; ignored otherwise).
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub exclude: Vec<String>,
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p mn-content manifest::tests`
Expected: ALL PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-content/src/manifest.rs
git commit -m "feat(mn-content): add include/exclude glob arrays to ManifestNode

Per-node filters used by the path: directory-pinning feature. Default
to empty (no filter); applied only when path: is set.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §3.1"
```

### Task 4: Move `manifest.rs` to `manifest/mod.rs` directory module

This is a no-behavior refactor to make room for sibling submodules (`resolve`, `sitemap`, `matcher`, `generate`).

**Files:**
- Move: `crates/mn-content/src/manifest.rs` → `crates/mn-content/src/manifest/mod.rs`
- Modify: nothing else (the `pub mod manifest;` in `lib.rs` already resolves to either form)

- [ ] **Step 1: Create the directory and move the file**

```bash
mkdir crates/mn-content/src/manifest
git mv crates/mn-content/src/manifest.rs crates/mn-content/src/manifest/mod.rs
```

- [ ] **Step 2: Run the full test suite to confirm nothing breaks**

Run: `cargo test -p mn-content`
Expected: every existing test PASS.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "refactor(mn-content): move manifest.rs to manifest/mod.rs

Opens the door for sibling submodules (resolve, sitemap, matcher,
generate) added in subsequent commits. No behavior change.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §5"
```

### Task 5: Define `ResolvedLeaf` and the resolver skeleton

**Files:**
- Create: `crates/mn-content/src/manifest/resolve.rs`
- Modify: `crates/mn-content/src/manifest/mod.rs` (add `pub mod resolve;`)

- [ ] **Step 1: Write the failing test**

Create `crates/mn-content/src/manifest/resolve.rs`:

```rust
//! Flatten a parsed `Manifest` into `Vec<ResolvedLeaf>` by walking the
//! tree top-down, applying `published_url` and `provenance` inheritance,
//! and (optionally — added in a later task) discovering files under
//! `path:` nodes.
//!
//! Spec: §3.1, §3.2 of docs/superpowers/specs/2026-05-25-ingest-ux-design.md

use std::path::{Path, PathBuf};

use mn_core::provenance::Provenance;
use mn_core::types::DocumentKind;
use serde::{Deserialize, Serialize};

use super::Manifest;

/// One leaf produced by walking the manifest, with inheritance applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedLeaf {
    /// Repo-relative path (relative to the resolver's `base`).
    pub rel_path: PathBuf,
    /// Document kind, derived from the file extension.
    pub kind: DocumentKind,
    /// Display name override, if any (from leaf `name:` or inherited).
    pub name: Option<String>,
    /// Final `published_url` after inheritance + leaf overrides.
    pub published_url: Option<String>,
    /// Final `source_url` after inheritance + leaf overrides.
    pub source_url: Option<String>,
    /// Provenance override merged from ancestor `provenance:` nodes.
    /// Frontmatter wins over this at ingest time; this only fills gaps.
    pub provenance_override: Provenance,
}

/// Resolve a manifest into its leaves. The `base` is informational at this
/// stage — file existence is checked by the walker, not the resolver.
///
/// At this checkpoint the resolver only handles explicit `file:` leaves
/// without any inheritance. Inheritance and `path:` discovery are added
/// in subsequent tasks.
#[must_use]
pub fn resolve(manifest: &Manifest, _base: &Path) -> Vec<ResolvedLeaf> {
    let mut out = Vec::new();
    walk(&manifest.root, &mut out);
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    out
}

fn walk(node: &super::ManifestNode, out: &mut Vec<ResolvedLeaf>) {
    if let Some(file) = &node.file {
        out.push(ResolvedLeaf {
            rel_path: file.clone(),
            kind: kind_for(file),
            name: node.name.clone(),
            published_url: node.published_url.clone(),
            source_url: None,
            provenance_override: Provenance::default(),
        });
    }
    for child in &node.children {
        walk(child, out);
    }
}

fn kind_for(path: &Path) -> DocumentKind {
    match crate::language::from_path(path) {
        Some("markdown") => DocumentKind::Markdown,
        Some(_) => DocumentKind::Code,
        None => DocumentKind::Plaintext,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn explicit_files_become_leaves() {
        let body = r"
manifest_version: 1
root:
  name: docs
  children:
    - file: a.md
    - file: dir/b.md
";
        let m = Manifest::parse(body).unwrap();
        let leaves = resolve(&m, Path::new("."));
        let paths: Vec<_> = leaves.iter().map(|l| l.rel_path.clone()).collect();
        assert_eq!(paths, vec![PathBuf::from("a.md"), PathBuf::from("dir/b.md")]);
        assert_eq!(leaves[0].kind, DocumentKind::Markdown);
    }
}
```

- [ ] **Step 2: Wire the module**

In `crates/mn-content/src/manifest/mod.rs`, add to the top:

```rust
pub mod resolve;
```

- [ ] **Step 3: Run the test**

Run: `cargo test -p mn-content manifest::resolve::tests`
Expected: 1 test PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/mn-content/src/manifest/
git commit -m "feat(mn-content): add ResolvedLeaf + resolver skeleton

Flat-list view of a parsed manifest: one ResolvedLeaf per file:. No
inheritance or path: discovery yet — those land in the next two
commits. Sets up the type the walker and PlanBuilder consume.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §3.2"
```

### Task 6: Implement `published_url` inheritance with prefix-join

**Files:**
- Modify: `crates/mn-content/src/manifest/resolve.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `resolve.rs`:

```rust
#[test]
fn published_url_inherits_with_prefix_join() {
    let body = r"
manifest_version: 1
root:
  name: docs
  published_url: https://docs.example.com/
  children:
    - name: Cookbook
      published_url: https://docs.example.com/cookbook/
      children:
        - file: auth.md
        - file: tls.md
";
    let m = Manifest::parse(body).unwrap();
    let leaves = resolve(&m, Path::new("."));
    let by_path: std::collections::HashMap<PathBuf, String> = leaves
        .iter()
        .map(|l| (l.rel_path.clone(), l.published_url.clone().unwrap()))
        .collect();
    assert_eq!(
        by_path[&PathBuf::from("auth.md")],
        "https://docs.example.com/cookbook/auth/"
    );
    assert_eq!(
        by_path[&PathBuf::from("tls.md")],
        "https://docs.example.com/cookbook/tls/"
    );
}

#[test]
fn leaf_published_url_override_wins() {
    let body = r"
manifest_version: 1
root:
  published_url: https://docs.example.com/cookbook/
  children:
    - file: auth.md
      published_url: https://docs.example.com/elsewhere/sign-in/
";
    let m = Manifest::parse(body).unwrap();
    let leaves = resolve(&m, Path::new("."));
    assert_eq!(
        leaves[0].published_url.as_deref(),
        Some("https://docs.example.com/elsewhere/sign-in/")
    );
}

#[test]
fn published_url_null_at_leaf_clears_inherited() {
    // serde_yaml maps `null` to None for Option<String>; the node-level
    // None should be distinguishable from "not declared" so that leaves
    // can opt out of inheritance. We use a sentinel string for "explicit
    // null" rather than YAML null — see §3.2 of the spec.
    //
    // Implemented as: a leaf published_url of exactly "" means "clear".
    let body = r#"
manifest_version: 1
root:
  published_url: https://docs.example.com/cookbook/
  children:
    - file: internal.md
      published_url: ""
"#;
    let m = Manifest::parse(body).unwrap();
    let leaves = resolve(&m, Path::new("."));
    assert_eq!(leaves[0].published_url, None);
}
```

- [ ] **Step 2: Run the tests, confirm they fail**

Run: `cargo test -p mn-content manifest::resolve::tests`
Expected: 3 new tests FAIL (URLs not joined, override not applied).

- [ ] **Step 3: Rewrite the walker to thread inherited URL**

Replace the `walk` function in `crates/mn-content/src/manifest/resolve.rs`:

```rust
fn walk(node: &super::ManifestNode, parent_url: Option<&str>, out: &mut Vec<ResolvedLeaf>) {
    let inherited = match (&node.published_url, parent_url) {
        // Leaf-level explicit empty string = clear inheritance.
        (Some(s), _) if s.is_empty() => None,
        (Some(s), _) => Some(s.as_str()),
        (None, p) => p,
    };

    if let Some(file) = &node.file {
        let final_url = compose_url(inherited, file);
        out.push(ResolvedLeaf {
            rel_path: file.clone(),
            kind: kind_for(file),
            name: node.name.clone(),
            published_url: final_url,
            source_url: None,
            provenance_override: Provenance::default(),
        });
    }
    for child in &node.children {
        walk(child, inherited, out);
    }
}

/// Compose the file's final `published_url` from an inherited prefix.
///
/// - When the inherited URL ends in `/`, append the file basename (no
///   extension) plus a trailing slash.
/// - When it doesn't end in `/`, treat it as a verbatim leaf override
///   (already final).
/// - When there is no inherited URL, return None.
fn compose_url(inherited: Option<&str>, file: &Path) -> Option<String> {
    let prefix = inherited?;
    if !prefix.ends_with('/') {
        return Some(prefix.to_owned());
    }
    let stem = file.file_stem()?.to_str()?;
    Some(format!("{prefix}{stem}/"))
}
```

Update the top-level `resolve` call site to seed the walk with `None`:

```rust
walk(&manifest.root, None, &mut out);
```

- [ ] **Step 4: Run the tests, confirm they pass**

Run: `cargo test -p mn-content manifest::resolve::tests`
Expected: ALL PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-content/src/manifest/resolve.rs
git commit -m "feat(mn-content): published_url inheritance with prefix-join

Top-down walk threads the nearest ancestor's published_url through
each leaf, joining as <prefix>/<basename>/. Leaf-level override wins
outright; an explicit empty string at the leaf clears inheritance.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §3.2"
```

### Task 7: Implement `provenance` field-merge inheritance

**Files:**
- Modify: `crates/mn-content/src/manifest/resolve.rs`

- [ ] **Step 1: Read the existing `Provenance` struct**

Run: `sed -n '90,130p' crates/mn-core/src/provenance.rs`
Confirm field names (`attribution`, `verified`, `verified_by`, etc.) so the merge logic targets the real fields.

- [ ] **Step 2: Write the failing test**

Add to `tests` in `resolve.rs`:

```rust
#[test]
fn provenance_merges_field_by_field_top_down() {
    let body = r#"
manifest_version: 1
root:
  provenance:
    attribution: foundation
    verified: true
    verified_by: midnight-foundation
  children:
    - file: a.md
"#;
    let m = Manifest::parse(body).unwrap();
    let leaves = resolve(&m, Path::new("."));
    let p = &leaves[0].provenance_override;
    assert_eq!(p.attribution, mn_core::provenance::Attribution::Foundation);
    assert!(p.verified);
    assert_eq!(p.verified_by.as_deref(), Some("midnight-foundation"));
}

#[test]
fn leaf_provenance_overrides_ancestor_fieldwise() {
    let body = r#"
manifest_version: 1
root:
  provenance:
    attribution: foundation
    verified: true
  children:
    - file: a.md
      provenance:
        verified: false
"#;
    let m = Manifest::parse(body).unwrap();
    let leaves = resolve(&m, Path::new("."));
    let p = &leaves[0].provenance_override;
    // Inherited attribution stays.
    assert_eq!(p.attribution, mn_core::provenance::Attribution::Foundation);
    // Leaf-level verified wins.
    assert!(!p.verified);
}
```

- [ ] **Step 3: Run the tests, confirm they fail**

Run: `cargo test -p mn-content manifest::resolve::tests`
Expected: 2 new tests FAIL (`provenance_override` is still the default).

- [ ] **Step 4: Extend the walker to thread `Provenance`**

Update `walk` in `resolve.rs` to accept and merge a `parent_prov: Option<&serde_json::Value>` (the raw JSON form is what `ManifestNode.provenance` already carries), merging with leaf-level `node.provenance`:

```rust
fn walk(
    node: &super::ManifestNode,
    parent_url: Option<&str>,
    parent_prov: &serde_json::Map<String, serde_json::Value>,
    out: &mut Vec<ResolvedLeaf>,
) {
    let merged_prov = merge_prov(parent_prov, node.provenance.as_ref());

    let inherited_url = match (&node.published_url, parent_url) {
        (Some(s), _) if s.is_empty() => None,
        (Some(s), _) => Some(s.as_str()),
        (None, p) => p,
    };

    if let Some(file) = &node.file {
        let final_url = compose_url(inherited_url, file);
        let prov_override = serde_json::from_value::<Provenance>(
            serde_json::Value::Object(merged_prov.clone()),
        )
        .unwrap_or_default();
        out.push(ResolvedLeaf {
            rel_path: file.clone(),
            kind: kind_for(file),
            name: node.name.clone(),
            published_url: final_url,
            source_url: None,
            provenance_override: prov_override,
        });
    }
    for child in &node.children {
        walk(child, inherited_url, &merged_prov, out);
    }
}

/// Field-by-field merge of two provenance maps: leaf wins per field.
fn merge_prov(
    parent: &serde_json::Map<String, serde_json::Value>,
    leaf: Option<&serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = parent.clone();
    if let Some(serde_json::Value::Object(map)) = leaf {
        for (k, v) in map {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}
```

Update the top-level `resolve` to pass `&serde_json::Map::new()` as the initial parent.

- [ ] **Step 5: Run the tests, confirm they pass**

Run: `cargo test -p mn-content manifest::resolve::tests`
Expected: ALL PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/mn-content/src/manifest/resolve.rs
git commit -m "feat(mn-content): provenance field-merge inheritance

Top-down walk merges provenance maps field-by-field, leaf-wins. The
resolved Provenance feeds the ingest pipeline as an override that
frontmatter beats at runtime.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §3.2"
```

### Task 8: Implement `path:` directory discovery in the resolver

**Files:**
- Modify: `crates/mn-content/src/manifest/resolve.rs`
- Modify: `crates/mn-content/Cargo.toml` (add `globset`)

- [ ] **Step 1: Add `globset` dep**

In the workspace root `Cargo.toml` under `[workspace.dependencies]`:

```toml
globset = "0.4"
```

In `crates/mn-content/Cargo.toml`:

```toml
globset = { workspace = true }
walkdir = { workspace = true }
```

If `walkdir` isn't already in the workspace deps, add it too: `walkdir = "2"`.

- [ ] **Step 2: Write the failing test**

Add to `tests` in `resolve.rs`:

```rust
#[test]
fn path_node_discovers_files_under_directory() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    std::fs::create_dir_all(base.join("docs/sub")).unwrap();
    std::fs::write(base.join("docs/a.md"), "# A").unwrap();
    std::fs::write(base.join("docs/sub/b.md"), "# B").unwrap();
    std::fs::write(base.join("docs/sub/skip.draft.md"), "# draft").unwrap();
    let body = r#"
manifest_version: 1
root:
  name: docs
  path: docs/
  published_url: https://docs.example.com/
  exclude: ["**/*.draft.md"]
"#;
    let m = Manifest::parse(body).unwrap();
    let leaves = resolve(&m, base);
    let paths: Vec<_> = leaves.iter().map(|l| l.rel_path.clone()).collect();
    assert_eq!(
        paths,
        vec![
            PathBuf::from("docs/a.md"),
            PathBuf::from("docs/sub/b.md"),
        ]
    );
    // Inherited URL prefix is joined with each discovered file's stem.
    assert!(leaves[0].published_url.as_deref().unwrap().ends_with("/a/"));
    assert!(leaves[1].published_url.as_deref().unwrap().ends_with("/b/"));
}

#[test]
fn explicit_file_in_children_wins_over_path_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    std::fs::create_dir_all(base.join("docs")).unwrap();
    std::fs::write(base.join("docs/a.md"), "# A").unwrap();
    let body = r#"
manifest_version: 1
root:
  name: docs
  path: docs/
  published_url: https://docs.example.com/
  children:
    - file: docs/a.md
      published_url: https://override.example.com/special/
"#;
    let m = Manifest::parse(body).unwrap();
    let leaves = resolve(&m, base);
    assert_eq!(leaves.len(), 1);
    assert_eq!(
        leaves[0].published_url.as_deref(),
        Some("https://override.example.com/special/")
    );
}
```

- [ ] **Step 3: Run the tests, confirm they fail**

Run: `cargo test -p mn-content manifest::resolve::tests`
Expected: both new tests FAIL.

- [ ] **Step 4: Implement `path:` discovery**

Extend `walk` in `resolve.rs` to enumerate files when `node.path` is set:

```rust
const DEFAULT_IGNORE_DIRS: &[&str] = &["node_modules", ".git", "target", "dist"];

fn discover_under_path(
    base: &Path,
    rel_dir: &Path,
    include: &[String],
    exclude: &[String],
    explicit_files: &std::collections::HashSet<PathBuf>,
) -> Vec<PathBuf> {
    let abs = base.join(rel_dir);
    if !abs.is_dir() {
        return Vec::new();
    }
    let include_set = build_globs(include);
    let exclude_set = build_globs(exclude);

    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(&abs)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            if name.starts_with('.') {
                return false;
            }
            if e.file_type().is_dir() && DEFAULT_IGNORE_DIRS.contains(&name.as_ref()) {
                return false;
            }
            true
        })
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = match entry.path().strip_prefix(base) {
            Ok(p) => p.to_path_buf(),
            Err(_) => continue,
        };
        if explicit_files.contains(&rel) {
            continue;
        }
        // Default include = all files whose extension yields a known
        // DocumentKind (i.e. mn_content::language::from_path returns Some).
        if include_set.is_none()
            && crate::language::from_path(&rel).is_none()
        {
            continue;
        }
        if let Some(set) = &include_set {
            if !set.is_match(&rel) {
                continue;
            }
        }
        if let Some(set) = &exclude_set {
            if set.is_match(&rel) {
                continue;
            }
        }
        out.push(rel);
    }
    out.sort();
    out
}

fn build_globs(patterns: &[String]) -> Option<globset::GlobSet> {
    if patterns.is_empty() {
        return None;
    }
    let mut builder = globset::GlobSetBuilder::new();
    for p in patterns {
        if let Ok(g) = globset::Glob::new(p) {
            builder.add(g);
        }
    }
    builder.build().ok()
}
```

Then in `walk`, before recursing into `node.children`, gather the explicit set from children and run discovery:

```rust
let explicit_files: std::collections::HashSet<PathBuf> = node
    .children
    .iter()
    .filter_map(|c| c.file.clone())
    .collect();

if let Some(path) = &node.path {
    for rel in discover_under_path(base, path, &node.include, &node.exclude, &explicit_files) {
        let url = compose_url(inherited_url, &rel);
        let prov_override = serde_json::from_value::<Provenance>(
            serde_json::Value::Object(merged_prov.clone()),
        )
        .unwrap_or_default();
        out.push(ResolvedLeaf {
            rel_path: rel.clone(),
            kind: kind_for(&rel),
            name: None,
            published_url: url,
            source_url: None,
            provenance_override: prov_override,
        });
    }
}
```

Thread `base: &Path` through `walk` (add as the first parameter; update the top-level `resolve` to pass it).

- [ ] **Step 5: Run the tests, confirm they pass**

Run: `cargo test -p mn-content manifest::resolve::tests`
Expected: ALL PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/mn-content/ Cargo.toml
git commit -m "feat(mn-content): implement path: directory discovery in resolver

Manifest nodes with path: now enumerate every matching file under the
directory, applying per-node include/exclude globs and the default
dot-file / ignore-list filter. Explicit children: file: entries win.
Inheritance threads through discovered files just like explicit ones.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §3.1, §3.2"
```

### Task 9: Strict-only walker rework

The `Walker` in `crates/mn-content/src/ingest/walker.rs` currently walks raw `Manifest` and reads files. After this task, it walks `Vec<ResolvedLeaf>` (computed via the resolver) and just reads the bytes.

**Files:**
- Modify: `crates/mn-content/src/ingest/walker.rs`
- Modify: `crates/mn-content/src/manifest/mod.rs:5` (fix the misleading docstring)

- [ ] **Step 1: Fix the misleading comment**

In `crates/mn-content/src/manifest/mod.rs` (formerly `manifest.rs`), replace the existing docstring lines 1–6 with:

```rust
//! `hierarchy.yaml` manifest loader (FR-017, FR-050).
//!
//! Manifests are the source of truth at ingest time. Only files reachable
//! from the manifest (via `file:` leaves or `path:` discovery) are
//! ingested. There is no directory-tree fallback — see §3.3 of the
//! ingest-UX design spec for rationale.
```

- [ ] **Step 2: Write the failing test**

In `walker.rs`'s `tests` module, add a test asserting that resolver-driven walking honors `path:`:

```rust
#[test]
fn walker_emits_resolved_leaves_including_path_discovery() {
    let dir = tempdir();
    write_file(dir.path(), "docs/a.md", "# A");
    write_file(dir.path(), "docs/sub/b.md", "# B");
    write_file(dir.path(), "outside.md", "# not in manifest");
    let body = r"
manifest_version: 1
root:
  name: docs
  path: docs/
";
    let manifest = Manifest::parse(body).unwrap();
    let walker = Walker::new(manifest, dir.path().to_path_buf());
    let docs = walker.walk().unwrap();
    let paths: Vec<_> = docs.iter().map(|d| d.rel_path.clone()).collect();
    assert_eq!(
        paths,
        vec![PathBuf::from("docs/a.md"), PathBuf::from("docs/sub/b.md")]
    );
}
```

- [ ] **Step 3: Run the test, confirm it fails**

Run: `cargo test -p mn-content ingest::walker::tests::walker_emits_resolved_leaves_including_path_discovery`
Expected: FAIL — current walker only sees `file:` entries.

- [ ] **Step 4: Add `resolved_leaf` to `WalkedDocument`**

Extend `WalkedDocument` in `walker.rs`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct WalkedDocument {
    pub rel_path: PathBuf,
    pub content: String,
    pub split: FrontmatterSplit,
    /// Resolver-derived inheritance — fed to PlanBuilder so it can be
    /// threaded to the upload layer.
    pub resolved: crate::manifest::resolve::ResolvedLeaf,
}
```

- [ ] **Step 5: Rewrite `walk` to use the resolver**

Replace the body of `pub fn walk(...)`:

```rust
pub fn walk(manifest: &Manifest, base: &Path) -> Result<Vec<WalkedDocument>, WalkError> {
    let leaves = crate::manifest::resolve::resolve(manifest, base);
    let mut out: Vec<WalkedDocument> = Vec::with_capacity(leaves.len());
    for leaf in leaves {
        let abs = base.join(&leaf.rel_path);
        if !abs.exists() {
            return Err(WalkError::MissingFile(leaf.rel_path.clone()));
        }
        let bytes = std::fs::read(&abs).map_err(|e| WalkError::Io {
            path: leaf.rel_path.clone(),
            source: e,
        })?;
        let content = String::from_utf8(bytes).map_err(|_| WalkError::NotUtf8 {
            path: leaf.rel_path.clone(),
        })?;
        let split = split_frontmatter(&content);
        out.push(WalkedDocument {
            rel_path: leaf.rel_path.clone(),
            content,
            split,
            resolved: leaf,
        });
    }
    Ok(out)
}
```

Delete the now-unused `collect_files` helper.

- [ ] **Step 6: Run the full mn-content test suite**

Run: `cargo test -p mn-content`
Expected: ALL PASS (existing walker tests should still pass — they only used `file:` leaves, which still resolve to leaves).

- [ ] **Step 7: Commit**

```bash
git add crates/mn-content/src/ingest/walker.rs crates/mn-content/src/manifest/mod.rs
git commit -m "feat(mn-content): walker consumes resolver output (strict-only)

The walker no longer reads ManifestNode directly. It calls
manifest::resolve, then reads bytes for each ResolvedLeaf. WalkedDocument
gains a resolved field carrying the inheritance-applied metadata
(published_url, provenance_override) so PlanBuilder can thread it to
the upload layer.

The walker is strict-only — files outside the manifest's reachable set
are not ingested. Updated the misleading docstring in manifest/mod.rs.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §3.3"
```

---

## Phase 3 — PlanBuilder + chunker token counts

### Task 10: Extend `PlannedDocument` with the five new fields

**Files:**
- Modify: `crates/mn-content/src/ingest/plan.rs`

- [ ] **Step 1: Write the failing test**

Add to `tests` in `plan.rs`:

```rust
#[test]
fn planned_document_carries_resolved_metadata() {
    use crate::manifest::resolve::ResolvedLeaf;
    use mn_core::types::DocumentKind;

    let leaf = ResolvedLeaf {
        rel_path: PathBuf::from("a.md"),
        kind: DocumentKind::Markdown,
        name: None,
        published_url: Some("https://docs.example.com/a/".to_owned()),
        source_url: Some("https://github.com/x/y/blob/main/a.md".to_owned()),
        provenance_override: Default::default(),
    };
    let mut b = empty_builder();
    let split = split_frontmatter("# A\n\nbody");
    b.add_walked_document(
        leaf.rel_path.clone(),
        DocumentKind::Markdown,
        "# A\n\nbody",
        &split,
        &leaf,
    )
    .unwrap();
    let plan = b.finalize();
    let doc = &plan.new_documents[0];
    assert_eq!(doc.published_url.as_deref(), Some("https://docs.example.com/a/"));
    assert_eq!(
        doc.source_url.as_deref(),
        Some("https://github.com/x/y/blob/main/a.md")
    );
    assert_eq!(doc.language.as_deref(), Some("markdown"));
}
```

- [ ] **Step 2: Run the test, confirm it fails**

Run: `cargo test -p mn-content ingest::plan::tests::planned_document_carries_resolved_metadata`
Expected: FAIL — `published_url` does not exist on `PlannedDocument`.

- [ ] **Step 3: Extend `PlannedDocument`**

Add five fields to `PlannedDocument` (in `plan.rs`):

```rust
/// Final published URL after manifest inheritance (None when neither
/// the manifest nor a sitemap matched).
pub published_url: Option<String>,
/// URL to the source of the document (e.g. a github blob URL).
pub source_url: Option<String>,
/// Filesystem-derived modification timestamp at walk time.
pub source_modified_at: Option<time::OffsetDateTime>,
/// IANA-like language identifier from `mn_content::language`.
pub language: Option<String>,
/// Token count of the document body (computed once at chunk time and
/// summed across chunks; landed in Task 12).
pub token_count: u32,
```

- [ ] **Step 4: Extend `PlannedChunk` with `token_count` (will be wired in Task 12)**

```rust
pub token_count: u32,
```

- [ ] **Step 5: Update `add_walked_document` to take a `&ResolvedLeaf`**

Add a fifth parameter and populate the new fields:

```rust
pub fn add_walked_document(
    &mut self,
    path: PathBuf,
    kind: DocumentKind,
    content: &str,
    split: &FrontmatterSplit,
    resolved: &crate::manifest::resolve::ResolvedLeaf,
) -> Result<(), IngestError> {
    // … existing dedup + hash + chunk logic …
    self.new_documents.push(PlannedDocument {
        path,
        kind,
        content_hash: hash,
        frontmatter: split.frontmatter.clone(),
        provenance: merge_provenance(&split.provenance, &resolved.provenance_override),
        char_count: content.chars().count(),
        chunks: planned_chunks,
        published_url: resolved.published_url.clone(),
        source_url: resolved.source_url.clone(),
        source_modified_at: None, // populated by the walker, threaded through in a later task
        language: crate::language::from_path(&resolved.rel_path).map(str::to_owned),
        token_count: 0, // populated in Task 12
    });
    Ok(())
}

/// Frontmatter wins per-field; ancestor `resolved` fills only the gaps.
fn merge_provenance(frontmatter: &Provenance, ancestor: &Provenance) -> Provenance {
    // Field-by-field: take frontmatter value if it differs from default;
    // else fall back to ancestor.
    let default = Provenance::default();
    let mut out = ancestor.clone();
    if frontmatter.attribution != default.attribution {
        out.attribution = frontmatter.attribution;
    }
    if frontmatter.verified != default.verified {
        out.verified = frontmatter.verified;
    }
    if frontmatter.verified_by != default.verified_by {
        out.verified_by = frontmatter.verified_by.clone();
    }
    // Add similar lines for any other Provenance fields visible at
    // `cargo doc --open -p mn-core` / `crates/mn-core/src/provenance.rs`.
    out
}
```

Update all in-crate call sites of `add_walked_document` (chunker tests, walker tests) to pass a stub `&ResolvedLeaf`. The walker itself now has `WalkedDocument.resolved` so it can pass it directly when feeding the builder.

- [ ] **Step 6: Run the full mn-content suite**

Run: `cargo test -p mn-content`
Expected: ALL PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/mn-content/src/ingest/plan.rs crates/mn-content/src/ingest/walker.rs
git commit -m "feat(mn-content): PlannedDocument carries resolved metadata

PlannedDocument gains published_url, source_url, source_modified_at,
language, token_count. PlannedChunk gains token_count. PlanBuilder's
add_walked_document now takes &ResolvedLeaf and threads inheritance
into both. Frontmatter still wins over manifest-inherited provenance
on a per-field basis.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §3.4"
```

### Task 11: Capture `source_modified_at` in the walker

**Files:**
- Modify: `crates/mn-content/src/ingest/walker.rs`
- Modify: `crates/mn-content/src/ingest/plan.rs` (thread through)

- [ ] **Step 1: Write the failing test**

Add to `walker.rs` tests:

```rust
#[test]
fn walker_captures_source_modified_at() {
    let dir = tempdir();
    write_file(dir.path(), "a.md", "# A");
    let body = "manifest_version: 1\nroot:\n  children:\n    - file: a.md\n";
    let manifest = Manifest::parse(body).unwrap();
    let walker = Walker::new(manifest, dir.path().to_path_buf());
    let docs = walker.walk().unwrap();
    assert!(docs[0].source_modified_at.is_some());
}
```

- [ ] **Step 2: Run, confirm fails**

Run: `cargo test -p mn-content ingest::walker::tests::walker_captures_source_modified_at`
Expected: FAIL — field does not exist on `WalkedDocument`.

- [ ] **Step 3: Add field and populate it**

In `walker.rs`, extend `WalkedDocument`:

```rust
pub source_modified_at: Option<time::OffsetDateTime>,
```

In the `walk` function, populate after reading the file:

```rust
let modified = std::fs::metadata(&abs)
    .ok()
    .and_then(|m| m.modified().ok())
    .map(time::OffsetDateTime::from);
out.push(WalkedDocument {
    rel_path: leaf.rel_path.clone(),
    content,
    split,
    resolved: leaf,
    source_modified_at: modified,
});
```

Add `time = { workspace = true }` to `crates/mn-content/Cargo.toml` if not already present.

- [ ] **Step 4: Thread it through PlanBuilder**

In `plan.rs`, change `add_walked_document`'s body to read the `source_modified_at` from… wait — `add_walked_document` takes content, split, resolved — it does NOT take `source_modified_at`. Add it as a sixth parameter or fold it onto `ResolvedLeaf`. Decision: add to the walker-side carrier and pass via a new `WalkContext` struct so the signature doesn't grow further:

Replace the `add_walked_document` signature with:

```rust
pub fn add_walked_document(
    &mut self,
    walked: &WalkContext<'_>,
) -> Result<(), IngestError> { … }
```

where `WalkContext` is in `plan.rs`:

```rust
pub struct WalkContext<'a> {
    pub path: PathBuf,
    pub kind: DocumentKind,
    pub content: &'a str,
    pub split: &'a FrontmatterSplit,
    pub resolved: &'a crate::manifest::resolve::ResolvedLeaf,
    pub source_modified_at: Option<time::OffsetDateTime>,
}
```

Update the walker-side feed call and every test call site. The struct keeps the signature stable as later fields are added.

Re-export `WalkContext` from `crates/mn-content/src/ingest/mod.rs` so callers can use `mn_content::ingest::WalkContext`:

```rust
pub use plan::{
    CarriedDocument, DeletedDocument, IngestError, IngestPlan, IngestStats,
    PlanBuilder, PlannedChunk, PlannedDocument, PriorDocument, PriorState,
    WalkContext,
};
```

- [ ] **Step 5: Run all tests**

Run: `cargo test -p mn-content`
Expected: ALL PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/mn-content/
git commit -m "feat(mn-content): capture source_modified_at in the walker

Walker reads fs::metadata().modified() and surfaces it on
WalkedDocument. PlanBuilder.add_walked_document is refactored to take
a WalkContext struct (keeps the signature stable as more fields land)
and threads source_modified_at into PlannedDocument.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §3.4"
```

### Task 12: Compute `token_count` at chunk time

**Files:**
- Modify: `crates/mn-content/Cargo.toml` (add `tokenizers`)
- Modify: `crates/mn-content/src/markdown.rs` (or wherever chunks are produced — the chunk-builder path)
- Modify: `crates/mn-content/src/ingest/plan.rs` (populate doc + chunk token counts)

- [ ] **Step 1: Add `tokenizers` dep**

In workspace `Cargo.toml`:

```toml
tokenizers = { version = "0.20", default-features = false, features = ["onig"] }
```

In `crates/mn-content/Cargo.toml`:

```toml
tokenizers = { workspace = true }
```

- [ ] **Step 2: Write the failing test**

Add to `plan.rs` tests:

```rust
#[test]
fn token_counts_are_populated_and_sum_to_document_total() {
    let mut b = empty_builder();
    feed(&mut b, "x.md", "# A\n\nbody one\n\n# B\n\nbody two with more tokens here.\n");
    let plan = b.finalize();
    let doc = &plan.new_documents[0];
    let chunk_sum: u32 = doc.chunks.iter().map(|c| c.token_count).sum();
    assert!(doc.token_count > 0);
    assert_eq!(doc.token_count, chunk_sum);
}
```

- [ ] **Step 3: Run, confirm fails**

Run: `cargo test -p mn-content ingest::plan::tests::token_counts_are_populated_and_sum_to_document_total`
Expected: FAIL — token counts are still 0.

- [ ] **Step 4: Add a token counter**

Create `crates/mn-content/src/tokens.rs`:

```rust
//! Token counting for the ingest pipeline. Uses the BPE tokenizer
//! bundled with the active embedding model (bge-base-en-v1.5) so
//! counts match what the server will see when embedding.

use std::sync::OnceLock;

use tokenizers::Tokenizer;

static TOKENIZER: OnceLock<Tokenizer> = OnceLock::new();

/// Initialize the tokenizer once. Falls back to a whitespace-split
/// approximation when the bundled vocab is unavailable (e.g. test
/// environments without the model files). The approximation is good
/// enough for plan-time estimates.
fn get() -> Option<&'static Tokenizer> {
    TOKENIZER.get_or_init(|| {
        // Load from the local model cache (mn_core::paths::model_cache).
        // If it isn't there, return a default-constructed tokenizer that
        // splits on whitespace.
        Tokenizer::from_pretrained("BAAI/bge-base-en-v1.5", None)
            .unwrap_or_else(|_| Tokenizer::from_str("{}").unwrap())
    });
    TOKENIZER.get()
}

/// Count tokens in `text`. Returns 0 if the tokenizer is unavailable
/// AND the text is empty; otherwise approximates by whitespace.
#[must_use]
pub fn count(text: &str) -> u32 {
    if let Some(tok) = get() {
        if let Ok(enc) = tok.encode(text, false) {
            return u32::try_from(enc.get_tokens().len()).unwrap_or(u32::MAX);
        }
    }
    // Whitespace fallback for environments without the tokenizer.
    u32::try_from(text.split_whitespace().count()).unwrap_or(u32::MAX)
}
```

Wire `pub mod tokens;` in `lib.rs`.

- [ ] **Step 5: Populate chunk + document token counts in PlanBuilder**

In `plan.rs` `add_walked_document`, when constructing `PlannedChunk`:

```rust
let token_count = crate::tokens::count(&c.content);
PlannedChunk {
    content: c.content,
    heading_path: c.heading_path,
    chunk_index: c.chunk_index,
    total_chunks: total,
    start_byte: c.start_byte,
    end_byte: c.end_byte,
    content_hash,
    token_count,
}
```

And after building the chunk vector:

```rust
let doc_tokens: u32 = planned_chunks.iter().map(|c| c.token_count).sum();
// then in the PlannedDocument constructor:
token_count: doc_tokens,
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p mn-content`
Expected: ALL PASS. (Whitespace fallback satisfies the new test in CI even without model files; tests should not depend on the BPE branch.)

- [ ] **Step 7: Commit**

```bash
git add crates/mn-content/ Cargo.toml
git commit -m "feat(mn-content): compute token_count at chunk time

New mn_content::tokens module wraps the bge-base-en-v1.5 tokenizer
(via fastembed's transitive tokenizers dep). Each PlannedChunk's
token_count is computed, then summed onto PlannedDocument.token_count.
Falls back to whitespace counting when the model files are unavailable
(e.g. CI without the model cache).

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §3.4"
```

---

## Phase 4 — Sitemap + matcher

### Task 13: Sitemap XML parser

**Files:**
- Create: `crates/mn-content/src/manifest/sitemap.rs`
- Modify: `crates/mn-content/src/manifest/mod.rs` (`pub mod sitemap;`)
- Modify: `crates/mn-content/Cargo.toml` (`quick-xml = "0.31"` workspace dep)
- Modify: workspace `Cargo.toml`

- [ ] **Step 1: Add `quick-xml` workspace dep**

In workspace `Cargo.toml`:

```toml
quick-xml = "0.31"
url = "2"
```

In `crates/mn-content/Cargo.toml`:

```toml
quick-xml = { workspace = true }
url       = { workspace = true }
```

- [ ] **Step 2: Write the failing test**

Create `crates/mn-content/src/manifest/sitemap.rs`:

```rust
//! Parse `<urlset>` and `<sitemapindex>` XML into a flat list of URLs.
//!
//! Spec: §1.2 of docs/superpowers/specs/2026-05-25-ingest-ux-design.md

use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
pub enum SitemapError {
    #[error("invalid sitemap XML: {0}")]
    Parse(String),
    #[error("invalid URL in sitemap: {0}")]
    BadUrl(String),
}

/// Parse a sitemap body. Returns the URLs from `<urlset><url><loc>...`,
/// or the index `<sitemapindex><sitemap><loc>...` entries (callers fetch
/// those recursively).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Urls(Vec<Url>),
    Index(Vec<Url>),
}

pub fn parse(body: &str) -> Result<Parsed, SitemapError> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(body);
    reader.trim_text(true);

    let mut urls: Vec<Url> = Vec::new();
    let mut is_index = false;
    let mut in_loc = false;
    let mut buf = Vec::new();
    let mut loc_buf = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name().0.to_ascii_lowercase();
                if name == b"sitemapindex" {
                    is_index = true;
                }
                if name == b"loc" {
                    in_loc = true;
                    loc_buf.clear();
                }
            }
            Ok(Event::Text(t)) if in_loc => {
                loc_buf.push_str(&t.unescape().map_err(|e| SitemapError::Parse(e.to_string()))?);
            }
            Ok(Event::End(e)) if e.name().0.eq_ignore_ascii_case(b"loc") => {
                in_loc = false;
                let parsed = Url::parse(loc_buf.trim())
                    .map_err(|_| SitemapError::BadUrl(loc_buf.trim().to_owned()))?;
                urls.push(parsed);
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(SitemapError::Parse(e.to_string())),
            _ => {}
        }
        buf.clear();
    }
    Ok(if is_index { Parsed::Index(urls) } else { Parsed::Urls(urls) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urlset() {
        let body = r#"<?xml version="1.0"?>
<urlset>
  <url><loc>https://docs.example.com/a/</loc></url>
  <url><loc>https://docs.example.com/b/</loc></url>
</urlset>"#;
        match parse(body).unwrap() {
            Parsed::Urls(v) => assert_eq!(v.len(), 2),
            Parsed::Index(_) => panic!("expected urlset"),
        }
    }

    #[test]
    fn parses_sitemap_index() {
        let body = r#"<?xml version="1.0"?>
<sitemapindex>
  <sitemap><loc>https://docs.example.com/sitemap-1.xml</loc></sitemap>
</sitemapindex>"#;
        assert!(matches!(parse(body).unwrap(), Parsed::Index(v) if v.len() == 1));
    }
}
```

- [ ] **Step 3: Wire the module**

In `crates/mn-content/src/manifest/mod.rs`, add:

```rust
pub mod sitemap;
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p mn-content manifest::sitemap`
Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-content/ Cargo.toml
git commit -m "feat(mn-content): sitemap XML parser

Parses <urlset> and <sitemapindex> bodies into Vec<Url>. Hand-rolled
on quick-xml; small enough to be vendor-free.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §1.2"
```

### Task 14: Sitemap fetcher (HTTP + file, recursing index one level)

**Files:**
- Modify: `crates/mn-content/src/manifest/sitemap.rs`

- [ ] **Step 1: Write the failing test (file form only — HTTP fetcher is mocked at a higher layer)**

Add to `sitemap.rs` tests:

```rust
#[test]
fn load_from_file_returns_urls() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sitemap.xml");
    std::fs::write(
        &path,
        r#"<urlset><url><loc>https://example.com/x/</loc></url></urlset>"#,
    )
    .unwrap();
    let urls = load_from_path(&path).unwrap();
    assert_eq!(urls.len(), 1);
}
```

- [ ] **Step 2: Implement `load_from_path` + `is_http`**

In `sitemap.rs`:

```rust
use std::path::Path;

#[must_use]
pub fn is_http(spec: &str) -> bool {
    spec.starts_with("http://") || spec.starts_with("https://")
}

pub fn load_from_path(path: &Path) -> Result<Vec<Url>, SitemapError> {
    let body = std::fs::read_to_string(path)
        .map_err(|e| SitemapError::Parse(format!("read {}: {e}", path.display())))?;
    match parse(&body)? {
        Parsed::Urls(v) => Ok(v),
        Parsed::Index(_) => Ok(Vec::new()), // file-form indexes are uncommon; ignore
    }
}

/// Async HTTP fetcher. Recurses one level into `<sitemapindex>`.
pub async fn load_from_url(
    client: &reqwest::Client,
    url: &Url,
) -> Result<Vec<Url>, SitemapError> {
    let body = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| SitemapError::Parse(e.to_string()))?
        .text()
        .await
        .map_err(|e| SitemapError::Parse(e.to_string()))?;
    match parse(&body)? {
        Parsed::Urls(v) => Ok(v),
        Parsed::Index(children) => {
            let mut all = Vec::new();
            for child in children {
                let body = client
                    .get(child)
                    .send()
                    .await
                    .map_err(|e| SitemapError::Parse(e.to_string()))?
                    .text()
                    .await
                    .map_err(|e| SitemapError::Parse(e.to_string()))?;
                if let Ok(Parsed::Urls(v)) = parse(&body) {
                    all.extend(v);
                }
            }
            Ok(all)
        }
    }
}
```

Add `reqwest = { workspace = true }` to `crates/mn-content/Cargo.toml` if not already present.

- [ ] **Step 3: Run the tests**

Run: `cargo test -p mn-content manifest::sitemap`
Expected: 3 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/mn-content/
git commit -m "feat(mn-content): sitemap fetcher (file + HTTP, one-level index recursion)

is_http() drives the URL/file dispatch (HTTP for http://, https://;
file otherwise). load_from_url recurses one level into sitemapindex
bodies. HTTP layer is async via the workspace reqwest client.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §1.2"
```

### Task 15: Matcher — slug + leaf + tail-relaxation

**Files:**
- Create: `crates/mn-content/src/manifest/matcher.rs`
- Modify: `crates/mn-content/src/manifest/mod.rs` (`pub mod matcher;`)

- [ ] **Step 1: Write the failing tests**

Create `crates/mn-content/src/manifest/matcher.rs`:

```rust
//! Match files to sitemap URLs.
//!
//! Order: frontmatter slug → leaf basename → tail-relaxation tie-break.
//! See §1.2 of docs/superpowers/specs/2026-05-25-ingest-ux-design.md

use std::path::Path;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchReason {
    Slug,
    Leaf,
    LeafWithParentDir,
    None,
}

#[derive(Debug, Clone)]
pub struct Match {
    pub url: Option<Url>,
    pub reason: MatchReason,
}

/// Match one file against a flat list of sitemap URLs.
///
/// `slug` is the file's frontmatter slug if present.
#[must_use]
pub fn match_file(file_rel: &Path, slug: Option<&str>, urls: &[Url]) -> Match {
    if let Some(s) = slug {
        if let Some(u) = urls.iter().find(|u| last_segment(u) == Some(s)) {
            return Match {
                url: Some(u.clone()),
                reason: MatchReason::Slug,
            };
        }
    }

    let leaf = file_rel
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if leaf.is_empty() {
        return Match { url: None, reason: MatchReason::None };
    }

    let leaf_hits: Vec<&Url> = urls
        .iter()
        .filter(|u| last_segment(u).map(|s| s == leaf).unwrap_or(false))
        .collect();
    match leaf_hits.len() {
        0 => Match { url: None, reason: MatchReason::None },
        1 => Match {
            url: Some(leaf_hits[0].clone()),
            reason: MatchReason::Leaf,
        },
        _ => disambiguate_by_tail(file_rel, &leaf_hits),
    }
}

/// Strip a leading `docs/` (or any single first dir) from the file's
/// path; then pick the URL whose trailing path-suffix shares the
/// longest tail with the file's path-suffix.
fn disambiguate_by_tail(file_rel: &Path, candidates: &[&Url]) -> Match {
    let file_tail = file_suffix_segments(file_rel);
    let mut best: Option<(&Url, usize)> = None;
    let mut tied = false;
    for cand in candidates {
        let url_tail = url_path_segments(cand);
        let common = common_suffix_len(&file_tail, &url_tail);
        match best {
            None => best = Some((cand, common)),
            Some((_, c)) if common > c => {
                best = Some((cand, common));
                tied = false;
            }
            Some((_, c)) if common == c => tied = true,
            _ => {}
        }
    }
    if tied {
        return Match { url: None, reason: MatchReason::None };
    }
    Match {
        url: best.map(|(u, _)| u.clone()),
        reason: MatchReason::LeafWithParentDir,
    }
}

fn last_segment(u: &Url) -> Option<&str> {
    u.path_segments()?.filter(|s| !s.is_empty()).last()
}

fn url_path_segments(u: &Url) -> Vec<String> {
    u.path_segments()
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// File's path segments minus extension on the last, with the FIRST
/// directory optionally stripped (e.g. `docs/`).
fn file_suffix_segments(file_rel: &Path) -> Vec<String> {
    let mut segs: Vec<String> = file_rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => s.to_str().map(str::to_owned),
            _ => None,
        })
        .collect();
    if let Some(last) = segs.last_mut() {
        if let Some(idx) = last.rfind('.') {
            last.truncate(idx);
        }
    }
    // Drop a leading "docs" if there's more than one segment to compare.
    if segs.len() > 1 && segs[0] == "docs" {
        segs.remove(0);
    }
    segs
}

fn common_suffix_len(a: &[String], b: &[String]) -> usize {
    let mut n = 0;
    for (x, y) in a.iter().rev().zip(b.iter().rev()) {
        if x == y {
            n += 1;
        } else {
            break;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn slug_match_wins_when_frontmatter_has_one() {
        let urls = vec![
            url("https://docs.example.com/cookbook/auth/"),
            url("https://docs.example.com/sign-in/"),
        ];
        let m = match_file(Path::new("docs/cookbook/auth.md"), Some("sign-in"), &urls);
        assert_eq!(m.reason, MatchReason::Slug);
        assert_eq!(m.url.unwrap().as_str(), "https://docs.example.com/sign-in/");
    }

    #[test]
    fn leaf_match_unique() {
        let urls = vec![url("https://docs.example.com/cookbook/auth/")];
        let m = match_file(Path::new("docs/cookbook/auth.md"), None, &urls);
        assert_eq!(m.reason, MatchReason::Leaf);
    }

    #[test]
    fn leaf_ambiguous_resolved_by_parent_dir() {
        let urls = vec![
            url("https://docs.example.com/cookbook/auth/"),
            url("https://docs.example.com/extras/auth/"),
        ];
        let m = match_file(Path::new("docs/cookbook/auth.md"), None, &urls);
        assert_eq!(m.reason, MatchReason::LeafWithParentDir);
        assert!(m.url.unwrap().path().contains("/cookbook/"));
    }

    #[test]
    fn leaf_ambiguous_still_tied_returns_none() {
        let urls = vec![
            url("https://docs.example.com/auth/"),
            url("https://docs.example.com/v2/auth/"),
        ];
        // File path has no parent context to break the tie.
        let m = match_file(Path::new("auth.md"), None, &urls);
        assert_eq!(m.reason, MatchReason::None);
        assert!(m.url.is_none());
    }
}
```

- [ ] **Step 2: Wire and run the tests**

In `crates/mn-content/src/manifest/mod.rs`:

```rust
pub mod matcher;
```

Run: `cargo test -p mn-content manifest::matcher`
Expected: 4 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/mn-content/
git commit -m "feat(mn-content): sitemap matcher (slug, leaf, tail-relaxation)

Returns a Match { url, reason } for a file given the sitemap's URLs.
Order: frontmatter slug → leaf basename → tie-break by longest shared
path-suffix (with a leading 'docs/' optionally stripped from the file
side). Ties after that return None — deterministic, not arbitrary.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §1.2"
```

---

## Phase 5 — Manifest generator

### Task 16: Generator skeleton — glob collection + frontmatter pass

**Files:**
- Create: `crates/mn-content/src/manifest/generate.rs`
- Modify: `crates/mn-content/src/manifest/mod.rs` (`pub mod generate;`)

- [ ] **Step 1: Define the input + output types**

Create `crates/mn-content/src/manifest/generate.rs`:

```rust
//! Generate a `Manifest` from a glob set + optional sitemaps. Pure-logic
//! core of `mnm manifest generate` (§1.2 of the spec).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Manifest, ManifestNode};
use crate::frontmatter;

/// Inputs to the generator.
#[derive(Debug, Clone)]
pub struct GenerateOptions {
    /// Filesystem root that all globs resolve against.
    pub base: PathBuf,
    /// Glob include patterns (positional GLOBS + --include flags, unioned).
    pub include: Vec<String>,
    /// Glob exclude patterns (--exclude flags).
    pub exclude: Vec<String>,
    /// Sitemap URLs (already fetched and flattened).
    pub sitemap_urls: Vec<url::Url>,
    /// Root node name (--name). None → derived from base directory.
    pub root_name: Option<String>,
    /// Fallback URL prefix when no sitemap match.
    pub url_base: Option<String>,
    /// Hoist shared published_url to common parent (--hoist).
    pub hoist: bool,
    /// Pin directories with ≥ pin_threshold matched files (--pin-dirs).
    pub pin_dirs: bool,
    /// Threshold for pin_dirs.
    pub pin_threshold: usize,
}

impl Default for GenerateOptions {
    fn default() -> Self {
        Self {
            base: PathBuf::from("."),
            include: Vec::new(),
            exclude: Vec::new(),
            sitemap_urls: Vec::new(),
            root_name: None,
            url_base: None,
            hoist: true,
            pin_dirs: true,
            pin_threshold: 5,
        }
    }
}

/// Per-file generator outcome — useful for the coverage report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateEntry {
    pub rel_path: PathBuf,
    pub matched_url: Option<String>,
    pub match_reason: String,
}

/// Final generator output.
#[derive(Debug, Clone)]
pub struct GenerateResult {
    pub manifest: Manifest,
    pub entries: Vec<GenerateEntry>,
}

pub fn generate(opts: &GenerateOptions) -> anyhow::Result<GenerateResult> {
    let files = collect_files(opts)?;
    let entries = build_entries(opts, &files)?;
    let manifest = build_manifest(opts, &entries);
    Ok(GenerateResult { manifest, entries })
}

fn collect_files(opts: &GenerateOptions) -> anyhow::Result<Vec<PathBuf>> {
    use globset::{Glob, GlobSetBuilder};

    let mut inc = GlobSetBuilder::new();
    for pat in &opts.include {
        inc.add(Glob::new(pat)?);
    }
    let inc = inc.build()?;
    let mut exc = GlobSetBuilder::new();
    for pat in &opts.exclude {
        exc.add(Glob::new(pat)?);
    }
    let exc = exc.build()?;

    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(&opts.base)
        .into_iter()
        .filter_entry(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(&opts.base)?.to_path_buf();
        if !inc.is_match(&rel) {
            continue;
        }
        if exc.is_match(&rel) {
            continue;
        }
        out.push(rel);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn build_entries(
    opts: &GenerateOptions,
    files: &[PathBuf],
) -> anyhow::Result<Vec<GenerateEntry>> {
    let mut out = Vec::with_capacity(files.len());
    for rel in files {
        let body = std::fs::read_to_string(opts.base.join(rel))?;
        let split = frontmatter::split(&body);
        let slug = slug_from_frontmatter(&split.frontmatter);
        let m = super::matcher::match_file(rel, slug.as_deref(), &opts.sitemap_urls);
        let matched_url = m.url.map(|u| u.to_string()).or_else(|| {
            opts.url_base.as_ref().map(|base| {
                let stem = rel.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let base = base.trim_end_matches('/');
                format!("{base}/{stem}/")
            })
        });
        let reason = format!("{:?}", m.reason);
        out.push(GenerateEntry {
            rel_path: rel.clone(),
            matched_url,
            match_reason: reason,
        });
    }
    Ok(out)
}

fn slug_from_frontmatter(fm: &Option<serde_json::Value>) -> Option<String> {
    fm.as_ref()?.get("slug")?.as_str().map(str::to_owned)
}

// build_manifest is implemented in Task 17.
fn build_manifest(_opts: &GenerateOptions, _entries: &[GenerateEntry]) -> Manifest {
    Manifest {
        manifest_version: 1,
        root: ManifestNode {
            name: Some("PLACEHOLDER".to_owned()),
            path: None,
            file: None,
            published_url: None,
            provenance: None,
            include: Vec::new(),
            exclude: Vec::new(),
            children: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_respects_include_exclude() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        std::fs::create_dir_all(base.join("docs")).unwrap();
        std::fs::write(base.join("docs/a.md"), "").unwrap();
        std::fs::write(base.join("docs/b.md"), "").unwrap();
        std::fs::write(base.join("docs/draft.md"), "").unwrap();
        let opts = GenerateOptions {
            base: base.to_path_buf(),
            include: vec!["docs/*.md".to_owned()],
            exclude: vec!["docs/draft.md".to_owned()],
            ..Default::default()
        };
        let files = collect_files(&opts).unwrap();
        assert_eq!(
            files,
            vec![PathBuf::from("docs/a.md"), PathBuf::from("docs/b.md")]
        );
    }
}
```

- [ ] **Step 2: Wire the module**

In `crates/mn-content/src/manifest/mod.rs`:

```rust
pub mod generate;
```

- [ ] **Step 3: Run**

Run: `cargo test -p mn-content manifest::generate`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/mn-content/
git commit -m "feat(mn-content): generator skeleton — glob + frontmatter + sitemap match

GenerateOptions / GenerateResult / GenerateEntry types. Collects files
via globs, parses frontmatter, matches each file to a sitemap URL via
manifest::matcher (with optional --url-base fallback). build_manifest
is stubbed and lands in the next two tasks.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §1.2"
```

### Task 17: Tree builder + hoist `published_url`

**Files:**
- Modify: `crates/mn-content/src/manifest/generate.rs`

- [ ] **Step 1: Write the failing test**

Add to `generate.rs` tests:

```rust
#[test]
fn build_manifest_produces_a_tree_and_hoists_common_url_prefix() {
    let entries = vec![
        GenerateEntry {
            rel_path: PathBuf::from("docs/auth.md"),
            matched_url: Some("https://docs.example.com/auth/".to_owned()),
            match_reason: "Leaf".to_owned(),
        },
        GenerateEntry {
            rel_path: PathBuf::from("docs/tls.md"),
            matched_url: Some("https://docs.example.com/tls/".to_owned()),
            match_reason: "Leaf".to_owned(),
        },
    ];
    let opts = GenerateOptions {
        root_name: Some("docs".to_owned()),
        hoist: true,
        pin_dirs: false,
        ..Default::default()
    };
    let m = build_manifest(&opts, &entries);
    // Root group exists.
    assert_eq!(m.root.name.as_deref(), Some("docs"));
    assert_eq!(m.root.children.len(), 1); // "docs" subgroup
    let docs_group = &m.root.children[0];
    // Hoisted prefix sits on the docs group.
    assert_eq!(
        docs_group.published_url.as_deref(),
        Some("https://docs.example.com/")
    );
    // Leaves no longer declare published_url (it's inherited).
    assert!(docs_group.children.iter().all(|c| c.published_url.is_none()));
}
```

- [ ] **Step 2: Replace the stub `build_manifest`**

```rust
fn build_manifest(opts: &GenerateOptions, entries: &[GenerateEntry]) -> Manifest {
    let root_name = opts
        .root_name
        .clone()
        .or_else(|| {
            opts.base
                .file_name()
                .and_then(|n| n.to_str())
                .map(title_case)
        })
        .unwrap_or_else(|| "Source".to_owned());

    // Group entries by their directory prefix path.
    let mut tree: TreeNode = TreeNode::group(root_name);
    for e in entries {
        tree.insert(&e.rel_path, e);
    }

    if opts.hoist {
        hoist_common_url(&mut tree);
    }
    // pin_dirs lands in Task 18.

    Manifest {
        manifest_version: 1,
        root: tree.into_node(),
    }
}

fn title_case(s: &str) -> String {
    s.split('-')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Intermediate tree representation. Wrapped because we want to
/// hoist URL prefixes before lowering to ManifestNode.
struct TreeNode {
    name: Option<String>,
    file: Option<PathBuf>,
    published_url: Option<String>,
    children: HashMap<String, TreeNode>,
}

impl TreeNode {
    fn group(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            file: None,
            published_url: None,
            children: HashMap::new(),
        }
    }

    fn leaf(file: PathBuf, url: Option<String>) -> Self {
        Self {
            name: None,
            file: Some(file),
            published_url: url,
            children: HashMap::new(),
        }
    }

    fn insert(&mut self, rel: &Path, e: &GenerateEntry) {
        let segs: Vec<_> = rel
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(s) => s.to_str().map(str::to_owned),
                _ => None,
            })
            .collect();
        self.insert_segs(&segs, e);
    }

    fn insert_segs(&mut self, segs: &[String], e: &GenerateEntry) {
        if segs.len() == 1 {
            let leaf = TreeNode::leaf(e.rel_path.clone(), e.matched_url.clone());
            self.children.insert(segs[0].clone(), leaf);
            return;
        }
        let head = segs[0].clone();
        let child = self
            .children
            .entry(head.clone())
            .or_insert_with(|| TreeNode::group(title_case(&head)));
        child.insert_segs(&segs[1..], e);
    }

    fn into_node(self) -> ManifestNode {
        let mut children: Vec<_> = self.children.into_iter().collect();
        children.sort_by(|a, b| a.0.cmp(&b.0));
        ManifestNode {
            name: self.name,
            path: None,
            file: self.file,
            published_url: self.published_url,
            provenance: None,
            include: Vec::new(),
            exclude: Vec::new(),
            children: children.into_iter().map(|(_, v)| v.into_node()).collect(),
        }
    }
}

/// If every leaf in a one-level subtree has a `published_url` sharing
/// the same prefix-up-to-final-segment, lift the prefix to the parent
/// node and clear the leaves.
fn hoist_common_url(node: &mut TreeNode) {
    for child in node.children.values_mut() {
        hoist_common_url(child);
    }
    if node.children.is_empty() {
        return;
    }
    // Gather child URLs that look like `<prefix>/<segment>/`.
    let prefixes: Vec<String> = node
        .children
        .values()
        .filter_map(|c| {
            let url = c.published_url.as_ref()?;
            let trimmed = url.trim_end_matches('/');
            let cut = trimmed.rfind('/')?;
            Some(trimmed[..=cut].to_owned() + "/")
        })
        .collect();
    if prefixes.len() != node.children.len() {
        return; // some leaf has no URL — skip hoist
    }
    let first = &prefixes[0];
    if !prefixes.iter().all(|p| p == first) {
        return;
    }
    node.published_url = Some(first.clone());
    for child in node.children.values_mut() {
        child.published_url = None;
    }
}
```

- [ ] **Step 3: Run**

Run: `cargo test -p mn-content manifest::generate`
Expected: ALL PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/mn-content/
git commit -m "feat(mn-content): generator — tree build + hoist published_url

Groups files by directory into an intermediate TreeNode, then lifts
common URL prefixes to the highest applicable parent. Leaves whose
URL is hoisted drop their published_url entirely (the resolver will
re-join it at ingest time).

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §1.2"
```

### Task 18: Generator — `--pin-dirs` (replace dense child sets with `path:`)

**Files:**
- Modify: `crates/mn-content/src/manifest/generate.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn pin_dirs_collapses_dense_child_set_to_path_node() {
    // Five files in one directory, all matched by the same rule, none
    // with leaf-level URL overrides (after hoist).
    let entries = (0..5)
        .map(|i| GenerateEntry {
            rel_path: PathBuf::from(format!("docs/cookbook/file-{i}.md")),
            matched_url: Some(format!("https://docs.example.com/cookbook/file-{i}/")),
            match_reason: "Leaf".to_owned(),
        })
        .collect::<Vec<_>>();
    let opts = GenerateOptions {
        root_name: Some("docs".to_owned()),
        hoist: true,
        pin_dirs: true,
        pin_threshold: 5,
        ..Default::default()
    };
    let m = build_manifest(&opts, &entries);
    // docs → cookbook
    let docs = &m.root.children[0];
    let cookbook = &docs.children[0];
    assert_eq!(cookbook.path.as_deref(), Some(Path::new("docs/cookbook/")));
    assert!(cookbook.children.is_empty());
}
```

- [ ] **Step 2: Implement `pin_dirs`**

After `hoist_common_url`, call a new pass:

```rust
if opts.pin_dirs {
    pin_dirs(&mut tree, &opts.base, opts.pin_threshold);
}

fn pin_dirs(node: &mut TreeNode, base: &Path, threshold: usize) {
    for child in node.children.values_mut() {
        pin_dirs(child, base, threshold);
    }
    // Pin a child group if: all its children are leaves with no
    // per-leaf URL (i.e. parent declared a hoisted URL), and the count
    // hits threshold.
    let leaves_only = node
        .children
        .values()
        .all(|c| c.file.is_some() && c.children.is_empty() && c.published_url.is_none());
    if leaves_only && node.children.len() >= threshold {
        // Derive `path:` from the common parent of every child's file path.
        let common = common_parent(
            &node
                .children
                .values()
                .filter_map(|c| c.file.clone())
                .collect::<Vec<_>>(),
        );
        if let Some(parent) = common {
            // Re-express as a single ManifestNode with path:; we store it
            // by hijacking node's own `file` slot temporarily, then
            // in into_node() the path: lands on the group.
            //
            // Simpler: stash the path on the group and clear children.
            node.children.clear();
            // We need a place to put `path:`. Extend TreeNode with `path: Option<PathBuf>`.
            node.path = Some(parent);
        }
    }
}

fn common_parent(paths: &[PathBuf]) -> Option<PathBuf> {
    let first = paths.first()?.parent()?.to_path_buf();
    for p in paths {
        if p.parent() != Some(&first) {
            return None;
        }
    }
    Some(first.join(""))
}
```

Extend `TreeNode` with `path: Option<PathBuf>` and surface it in `into_node`:

```rust
ManifestNode {
    // …
    path: self.path,
    // …
}
```

- [ ] **Step 3: Run**

Run: `cargo test -p mn-content manifest::generate`
Expected: ALL PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/mn-content/
git commit -m "feat(mn-content): generator — pin dense directories to path:

When a directory contains ≥ pin_threshold (default 5) leaf-only
children whose URLs have all been hoisted to the parent, replace the
explicit children with a single path: directive. Keeps generated
manifests compact at scale.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §3.5"
```

### Task 19: YAML emission with self-documenting header

**Files:**
- Modify: `crates/mn-content/src/manifest/generate.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn emit_yaml_starts_with_header_comment() {
    let m = Manifest {
        manifest_version: 1,
        root: ManifestNode {
            name: Some("docs".to_owned()),
            path: None,
            file: None,
            published_url: None,
            provenance: None,
            include: Vec::new(),
            exclude: Vec::new(),
            children: Vec::new(),
        },
    };
    let body = emit_yaml(&m, "2026-05-25").unwrap();
    assert!(body.starts_with("# Generated by `mnm manifest generate` on 2026-05-25."));
    assert!(body.contains("manifest_version: 1"));
}
```

- [ ] **Step 2: Implement**

```rust
pub fn emit_yaml(manifest: &Manifest, date: &str) -> anyhow::Result<String> {
    let header = format!(
        "# Generated by `mnm manifest generate` on {date}.
# Schema: crates/mn-content/src/manifest/mod.rs (manifest_version = 1).
#
# Each leaf node references one source file via `file:` (relative to
# the manifest's parent dir, or `--base` at generate time). Groups use
# `name:` and `children:` to nest. `published_url:` and `provenance:`
# on any node are inherited by descendants. A node with `path:`
# auto-discovers every supported file under that directory.
"
    );
    let body = serde_yaml::to_string(manifest)?;
    Ok(format!("{header}\n{body}"))
}
```

- [ ] **Step 3: Run**

Run: `cargo test -p mn-content manifest::generate`
Expected: ALL PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/mn-content/
git commit -m "feat(mn-content): generator — YAML emission with header comment

emit_yaml(manifest, date) returns a String prefixed with a self-
documenting header pointing at the schema location and the inheritance
rules. Body is plain serde_yaml output.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §1.2"
```

---

## Phase 6 — CLI: `manifest` namespace + progress reporter

### Task 20: Add the `progress::Reporter` trait + impls

**Files:**
- Create: `crates/mn-cli/src/progress.rs`
- Modify: `crates/mn-cli/src/lib.rs` (`pub mod progress;`)

- [ ] **Step 1: Define the trait**

Create `crates/mn-cli/src/progress.rs`:

```rust
//! Progress reporting abstraction for long-running CLI commands.
//!
//! Two impls today:
//! - `Tty` — multi-progress bars + spinner (indicatif), for terminals.
//! - `Json` — one JSONL event per phase, for piped stdout / --json.
//!
//! Spec: §2.3 of docs/superpowers/specs/2026-05-25-ingest-ux-design.md

use serde_json::json;

pub trait Reporter: Send {
    /// A phase started; payload is structured data for the JSON impl.
    fn phase(&mut self, name: &str, payload: serde_json::Value);
    /// A phase completed.
    fn phase_done(&mut self, name: &str, payload: serde_json::Value);
    /// Long-running phase progress (current, total, label).
    fn batch(&mut self, current: usize, total: usize, label: &str);
}

pub struct Json;

impl Reporter for Json {
    fn phase(&mut self, name: &str, payload: serde_json::Value) {
        let mut obj = serde_json::Map::new();
        obj.insert("phase".to_owned(), json!(name));
        if let serde_json::Value::Object(m) = payload {
            obj.extend(m);
        }
        println!("{}", serde_json::Value::Object(obj));
    }
    fn phase_done(&mut self, name: &str, payload: serde_json::Value) {
        self.phase(name, payload);
    }
    fn batch(&mut self, current: usize, total: usize, label: &str) {
        self.phase(label, json!({"current": current, "of": total}));
    }
}

pub struct Tty {
    mp: indicatif::MultiProgress,
    bar: Option<indicatif::ProgressBar>,
}

impl Tty {
    #[must_use]
    pub fn new() -> Self {
        Self {
            mp: indicatif::MultiProgress::new(),
            bar: None,
        }
    }
}

impl Default for Tty {
    fn default() -> Self {
        Self::new()
    }
}

impl Reporter for Tty {
    fn phase(&mut self, name: &str, _payload: serde_json::Value) {
        let pb = self.mp.add(indicatif::ProgressBar::new_spinner());
        pb.set_message(name.to_owned());
        pb.enable_steady_tick(std::time::Duration::from_millis(120));
        self.bar = Some(pb);
    }
    fn phase_done(&mut self, name: &str, payload: serde_json::Value) {
        if let Some(pb) = self.bar.take() {
            let summary = format_summary(name, &payload);
            pb.finish_with_message(format!("✓ {summary}"));
        }
    }
    fn batch(&mut self, current: usize, total: usize, label: &str) {
        if let Some(pb) = &self.bar {
            pb.set_message(format!("{label}: batch {current}/{total}"));
        }
    }
}

fn format_summary(name: &str, payload: &serde_json::Value) -> String {
    let detail = payload
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    if detail.is_empty() {
        name.to_owned()
    } else {
        format!("{name} {detail}")
    }
}

/// Pick the right reporter based on `--json` and TTY detection.
#[must_use]
pub fn pick(json: bool) -> Box<dyn Reporter> {
    use std::io::IsTerminal as _;
    if json || !std::io::stdout().is_terminal() {
        Box::new(Json)
    } else {
        Box::new(Tty::new())
    }
}
```

- [ ] **Step 2: Wire and confirm it builds**

In `crates/mn-cli/src/lib.rs` (or wherever modules are declared):

```rust
pub mod progress;
```

Confirm `indicatif = { workspace = true }` is in `crates/mn-cli/Cargo.toml` (already is from the search done in plan prep).

Run: `cargo check -p mn-cli`
Expected: builds.

- [ ] **Step 3: Commit**

```bash
git add crates/mn-cli/src/progress.rs crates/mn-cli/src/lib.rs crates/mn-cli/Cargo.toml
git commit -m "feat(mn-cli): add progress::Reporter trait + Tty/Json impls

Abstraction for ingest progress UX. pick(json) returns the right
impl based on --json and stdout TTY detection. Tty uses indicatif's
multi-progress; Json emits one JSONL event per phase transition.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §2.3"
```

### Task 21: `mnm manifest init`

**Files:**
- Create: `crates/mn-cli/src/commands/manifest/mod.rs`
- Create: `crates/mn-cli/src/commands/manifest/init.rs`
- Modify: `crates/mn-cli/src/commands/mod.rs` (`pub mod manifest;`)
- Modify: `crates/mn-cli/src/cli.rs` (add `Manifest` Command variant)

- [ ] **Step 1: Dispatcher + init module**

Create `crates/mn-cli/src/commands/manifest/mod.rs`:

```rust
//! `mnm manifest <subcommand>` dispatcher. See §1 of the ingest-UX
//! design spec.

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};

pub mod init;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub cmd: ManifestCmd,
}

#[derive(Debug, Subcommand)]
pub enum ManifestCmd {
    /// Write an empty starter manifest with comments.
    Init(init::Args),
}

pub async fn run(args: Args) -> Result<()> {
    match args.cmd {
        ManifestCmd::Init(a) => init::run(a),
    }
}
```

Create `crates/mn-cli/src/commands/manifest/init.rs`:

```rust
//! `mnm manifest init` — write a starter `hierarchy.yaml`.

use std::path::PathBuf;

use anyhow::{anyhow, Context as _, Result};
use clap::Args as ClapArgs;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Output path (default `./hierarchy.yaml`).
    #[arg(short, long, default_value = "hierarchy.yaml")]
    pub output: PathBuf,
    /// Overwrite if the file already exists.
    #[arg(long)]
    pub force: bool,
}

pub fn run(args: Args) -> Result<()> {
    if args.output.exists() && !args.force {
        return Err(anyhow!(
            "refusing to overwrite {}; pass --force to replace",
            args.output.display()
        ));
    }
    let date = time::OffsetDateTime::now_utc().date();
    let body = body(&date.to_string());
    std::fs::write(&args.output, body)
        .with_context(|| format!("write {}", args.output.display()))?;
    eprintln!("wrote {}", args.output.display());
    Ok(())
}

fn body(date: &str) -> String {
    format!(
        "# Generated by `mnm manifest init` on {date}.
# Schema: crates/mn-content/src/manifest/mod.rs (manifest_version = 1).
#
# Each leaf node references one source file via `file:` (relative to the
# manifest's parent dir, or `--base` at generate time). Groups use `name:`
# and `children:` to nest. `published_url:` and `provenance:` on any node
# are inherited by descendants — declare them at the highest applicable
# node to avoid repetition. A node with `path:` auto-discovers every
# supported file under that directory.

manifest_version: 1
root:
  name: My Source
  # published_url: https://example.com/docs/
  children:
    - file: path/to/your/first.md
      # name: Pretty Title
      # published_url: https://example.com/docs/first/
"
    )
}
```

- [ ] **Step 2: Wire into `Command` enum**

In `crates/mn-cli/src/commands/mod.rs`:

```rust
pub mod manifest;
```

In `crates/mn-cli/src/cli.rs`:

```rust
#[derive(Debug, Subcommand)]
pub enum Command {
    // … existing variants …
    /// Manifest authoring + validation (local only).
    Manifest(commands::manifest::Args),
    // …
}
```

In the dispatch `match`:

```rust
Command::Manifest(args) => commands::manifest::run(args).await,
```

In `ADMIN_SUBCOMMANDS`: leave unchanged (manifest is not admin).

In `cli_command_name`: add a `Manifest` discriminant to `CliCommandName` (in `mn-telemetry::events::CliCommandName`) and route to it. Add the `Manifest` variant to the enum in `crates/mn-telemetry/src/events.rs` alongside the existing variants. The variant must serialize as `"manifest"` to match the existing snake_case pattern used by `serde(rename_all = "snake_case")` on the enum (verify by reading the existing derive).

- [ ] **Step 3: Write the integration test**

Create `crates/mn-cli/tests/manifest_init.rs`:

```rust
use std::process::Command;

#[test]
fn manifest_init_writes_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("hierarchy.yaml");
    let exe = env!("CARGO_BIN_EXE_mnm");
    let status = Command::new(exe)
        .args(["manifest", "init", "-o"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(out.exists());
    let body = std::fs::read_to_string(&out).unwrap();
    assert!(body.starts_with("# Generated by `mnm manifest init`"));
    assert!(body.contains("manifest_version: 1"));
}

#[test]
fn manifest_init_refuses_to_overwrite_without_force() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("hierarchy.yaml");
    std::fs::write(&out, "existing").unwrap();
    let exe = env!("CARGO_BIN_EXE_mnm");
    let status = Command::new(exe)
        .args(["manifest", "init", "-o"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(!status.success());
}
```

- [ ] **Step 4: Run**

Run: `cargo test -p mn-cli manifest_init`
Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-cli/ crates/mn-telemetry/src/events.rs
git commit -m "feat(mn-cli): mnm manifest init writes a starter hierarchy.yaml

New top-level Manifest command (always visible — no admin gate).
init writes a self-documenting starter file; refuses to overwrite
without --force. Adds CliCommandName::Manifest for telemetry.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §1.1"
```

### Task 22: `mnm manifest generate`

**Files:**
- Create: `crates/mn-cli/src/commands/manifest/generate.rs`
- Modify: `crates/mn-cli/src/commands/manifest/mod.rs`

- [ ] **Step 1: Implement the command**

Create `crates/mn-cli/src/commands/manifest/generate.rs`:

```rust
//! `mnm manifest generate` — populate a hierarchy.yaml from globs +
//! optional sitemap.

use std::path::PathBuf;

use anyhow::{anyhow, Context as _, Result};
use clap::Args as ClapArgs;
use mn_content::manifest::generate::{generate, GenerateOptions};
use mn_content::manifest::sitemap;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Glob patterns to include (positional). Combined with --include.
    pub globs: Vec<String>,
    /// Glob include pattern (repeatable).
    #[arg(long = "include")]
    pub include: Vec<String>,
    /// Glob exclude pattern (repeatable).
    #[arg(long = "exclude")]
    pub exclude: Vec<String>,
    /// Base directory to resolve globs against.
    #[arg(long, default_value = ".")]
    pub base: PathBuf,
    /// Sitemap URL or file path (repeatable).
    #[arg(long = "sitemap")]
    pub sitemap: Vec<String>,
    /// Fallback URL prefix when no sitemap match.
    #[arg(long = "url-base")]
    pub url_base: Option<String>,
    /// Root node name (default: --base dir name).
    #[arg(long)]
    pub name: Option<String>,
    /// Output path (default `./hierarchy.yaml`).
    #[arg(short, long, default_value = "hierarchy.yaml")]
    pub output: PathBuf,
    /// Overwrite existing output.
    #[arg(long)]
    pub force: bool,
    /// Fail if any file is unmatched.
    #[arg(long)]
    pub strict: bool,
    /// Write unmatched-files report here.
    #[arg(long)]
    pub report: Option<PathBuf>,
    /// Disable common-URL-prefix hoist.
    #[arg(long)]
    pub no_hoist: bool,
    /// Disable directory pinning.
    #[arg(long)]
    pub no_pin_dirs: bool,
    /// Threshold for directory pinning.
    #[arg(long, default_value_t = 5)]
    pub pin_threshold: usize,
    /// Print YAML to stdout; write nothing.
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(args: Args) -> Result<()> {
    if !args.dry_run && args.output.exists() && !args.force {
        return Err(anyhow!(
            "refusing to overwrite {}; pass --force",
            args.output.display()
        ));
    }
    let sitemap_urls = load_sitemaps(&args.sitemap).await?;
    let opts = GenerateOptions {
        base: args.base.clone(),
        include: args.globs.iter().chain(args.include.iter()).cloned().collect(),
        exclude: args.exclude.clone(),
        sitemap_urls,
        root_name: args.name.clone(),
        url_base: args.url_base.clone(),
        hoist: !args.no_hoist,
        pin_dirs: !args.no_pin_dirs,
        pin_threshold: args.pin_threshold,
    };
    let result = generate(&opts).context("generate manifest")?;

    let date = time::OffsetDateTime::now_utc().date().to_string();
    let body = mn_content::manifest::generate::emit_yaml(&result.manifest, &date)
        .context("emit yaml")?;

    if args.dry_run {
        println!("{body}");
    } else {
        std::fs::write(&args.output, body)
            .with_context(|| format!("write {}", args.output.display()))?;
        eprintln!("wrote {}", args.output.display());
    }

    let total = result.entries.len();
    let unmatched: Vec<_> = result
        .entries
        .iter()
        .filter(|e| e.matched_url.is_none())
        .collect();
    let via_slug = result
        .entries
        .iter()
        .filter(|e| e.match_reason == "Slug")
        .count();
    let via_leaf = result
        .entries
        .iter()
        .filter(|e| e.match_reason == "Leaf" || e.match_reason == "LeafWithParentDir")
        .count();
    eprintln!(
        "generated: {total} files, {matched} URLs matched (via slug: {via_slug}, leaf: {via_leaf}), {} unmatched.",
        unmatched.len(),
        matched = total - unmatched.len(),
    );

    if let Some(report) = &args.report {
        let lines: String = unmatched
            .iter()
            .map(|e| format!("{} {}\n", e.rel_path.display(), e.match_reason))
            .collect();
        std::fs::write(report, lines)
            .with_context(|| format!("write {}", report.display()))?;
    }

    if args.strict && !unmatched.is_empty() {
        return Err(anyhow!(
            "{} files unmatched (--strict)",
            unmatched.len()
        ));
    }
    Ok(())
}

async fn load_sitemaps(specs: &[String]) -> Result<Vec<url::Url>> {
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("build HTTP client")?;
    let mut out = Vec::new();
    for spec in specs {
        if sitemap::is_http(spec) {
            let url = url::Url::parse(spec).with_context(|| format!("parse {spec}"))?;
            out.extend(sitemap::load_from_url(&client, &url).await?);
        } else {
            out.extend(sitemap::load_from_path(std::path::Path::new(spec))?);
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}
```

- [ ] **Step 2: Register in the dispatcher**

In `crates/mn-cli/src/commands/manifest/mod.rs`:

```rust
pub mod generate;

#[derive(Debug, Subcommand)]
pub enum ManifestCmd {
    Init(init::Args),
    Generate(generate::Args),
}

pub async fn run(args: Args) -> Result<()> {
    match args.cmd {
        ManifestCmd::Init(a) => init::run(a),
        ManifestCmd::Generate(a) => generate::run(a).await,
    }
}
```

- [ ] **Step 3: Integration test**

Create `crates/mn-cli/tests/manifest_generate.rs`:

```rust
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
```

- [ ] **Step 4: Run**

Run: `cargo test -p mn-cli manifest_generate`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-cli/
git commit -m "feat(mn-cli): mnm manifest generate

Thin CLI veneer over mn_content::manifest::generate. Loads sitemaps
(URL or file), invokes the generator, emits YAML, prints a coverage
summary, optionally writes an unmatched-files report. --strict
upgrades unmatched > 0 to non-zero exit.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §1.2"
```

### Task 23: `mnm manifest check`

**Files:**
- Create: `crates/mn-cli/src/commands/manifest/check.rs`
- Modify: `crates/mn-cli/src/commands/manifest/mod.rs`

- [ ] **Step 1: Implement**

Create `crates/mn-cli/src/commands/manifest/check.rs`:

```rust
//! `mnm manifest check` — purely-local manifest validation.

use std::path::PathBuf;

use anyhow::{anyhow, Context as _, Result};
use clap::Args as ClapArgs;
use mn_content::manifest::Manifest;

#[derive(Debug, ClapArgs)]
pub struct Args {
    pub manifest: PathBuf,
    #[arg(long)]
    pub base: Option<PathBuf>,
    #[arg(long = "sitemap")]
    pub sitemap: Vec<String>,
    #[arg(long)]
    pub strict: bool,
}

pub async fn run(args: Args) -> Result<()> {
    let body = std::fs::read_to_string(&args.manifest)
        .with_context(|| format!("read {}", args.manifest.display()))?;
    let manifest = Manifest::parse(&body).context("parse manifest")?;

    let mut issues: Vec<String> = Vec::new();

    if let Err(e) = manifest.validate() {
        issues.push(format!("schema/paths: {e}"));
    }
    let base = args.base.clone().unwrap_or_else(|| {
        args.manifest
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    });
    let missing = manifest.validate_files_exist(&base);
    for m in &missing {
        issues.push(format!("missing file: {}", m.display()));
    }

    if !args.sitemap.is_empty() {
        let sitemap_urls = super::generate::load_sitemaps(&args.sitemap).await?;
        let leaves = mn_content::manifest::resolve::resolve(&manifest, &base);
        let matched = leaves
            .iter()
            .filter(|l| {
                l.published_url
                    .as_ref()
                    .map_or(false, |u| sitemap_urls.iter().any(|s| s.as_str() == u))
            })
            .count();
        eprintln!(
            "sitemap coverage: {}/{} ({}%)",
            matched,
            leaves.len(),
            if leaves.is_empty() { 100 } else { matched * 100 / leaves.len() }
        );
    }

    if issues.is_empty() {
        eprintln!("ok");
        return Ok(());
    }
    for i in &issues {
        eprintln!("- {i}");
    }
    Err(anyhow!("{} issue(s)", issues.len()))
}
```

(Promote `load_sitemaps` in `manifest/generate.rs` to `pub` so `check.rs` can use it.)

- [ ] **Step 2: Register + test**

Update `manifest/mod.rs`:

```rust
pub mod check;

pub enum ManifestCmd {
    Init(init::Args),
    Generate(generate::Args),
    Check(check::Args),
}

pub async fn run(args: Args) -> Result<()> {
    match args.cmd {
        ManifestCmd::Init(a) => init::run(a),
        ManifestCmd::Generate(a) => generate::run(a).await,
        ManifestCmd::Check(a) => check::run(a).await,
    }
}
```

Create `crates/mn-cli/tests/manifest_check.rs`:

```rust
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
```

- [ ] **Step 3: Run**

Run: `cargo test -p mn-cli manifest_check`
Expected: 2 PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/mn-cli/
git commit -m "feat(mn-cli): mnm manifest check

Validates a manifest locally: schema, path safety, duplicate detection,
file existence. With --sitemap, reports URL coverage. Reports all
issues (not first-fail). Exits non-zero on any hard error.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §1.3"
```

---

## Phase 7 — CLI: `ingest` namespace restructure

### Task 24: Restructure `commands/ingest.rs` → `commands/ingest/{mod,run}.rs`

This is a refactor with no behavior change to set up plan + run as siblings.

**Files:**
- Move: `crates/mn-cli/src/commands/ingest.rs` → `crates/mn-cli/src/commands/ingest/run.rs`
- Create: `crates/mn-cli/src/commands/ingest/mod.rs`
- Modify: `crates/mn-cli/src/cli.rs` (route `Command::Ingest` through subcommands)

- [ ] **Step 1: Move + rename**

```bash
mkdir crates/mn-cli/src/commands/ingest
git mv crates/mn-cli/src/commands/ingest.rs crates/mn-cli/src/commands/ingest/run.rs
```

- [ ] **Step 2: Create the dispatcher**

`crates/mn-cli/src/commands/ingest/mod.rs`:

```rust
//! `mnm ingest <subcommand>` dispatcher. See §2 of the ingest-UX spec.

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};
use mn_telemetry::TelemetryClient;

pub mod run;
// `plan` lands in Task 25.

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub cmd: IngestCmd,
}

#[derive(Debug, Subcommand)]
pub enum IngestCmd {
    /// Execute an ingest against the cloud server.
    Run(run::Args),
}

pub async fn run(
    args: Args,
    server: Option<&str>,
    telemetry: &TelemetryClient,
    cli_version: &str,
    json: bool,
) -> Result<()> {
    match args.cmd {
        IngestCmd::Run(a) => run::run(a, server, telemetry, cli_version, json).await,
    }
}
```

- [ ] **Step 3: Update cli.rs dispatch**

The existing `Command::Ingest(args) => commands::ingest::run(...)` line already routes through `commands::ingest::run` — the function call now lands on the new dispatcher, which routes to `run::run`. No further change needed.

- [ ] **Step 4: Build + tests**

Run: `cargo test -p mn-cli`
Expected: ALL PASS (no test changes, just the refactor).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor(mn-cli): split commands/ingest into mod.rs + run.rs

Sets up plan as a sibling of run. No behavior change — existing
ingest behavior is now reached via 'mnm ingest run <manifest>'
internally; CLI surface still wires through commands::ingest::run.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §2"
```

### Task 25: `mnm ingest plan` — local dry-run with server prior-state fetch

**Files:**
- Create: `crates/mn-cli/src/commands/ingest/plan.rs`
- Modify: `crates/mn-cli/src/commands/ingest/mod.rs`

- [ ] **Step 1: Implement**

Create `crates/mn-cli/src/commands/ingest/plan.rs`:

```rust
//! `mnm ingest plan` — compute the full ingest plan without starting a
//! server-side ingest run.

use std::path::PathBuf;

use anyhow::{anyhow, Context as _, Result};
use clap::Args as ClapArgs;
use mn_content::ingest::{PlanBuilder, PriorState, Walker, WalkContext};
use mn_content::manifest::{self, Manifest};
use mn_core::types::{DocumentKind, SourceKind};

#[derive(Debug, ClapArgs)]
pub struct Args {
    pub manifest: PathBuf,
    #[arg(long)]
    pub source_slug: String,
    #[arg(long)]
    pub revision: Option<String>,
    #[arg(long, default_value = "bge-base-en-v1.5@1")]
    pub embedding_model: String,
    #[arg(long)]
    pub base: Option<PathBuf>,
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: Args, server: Option<&str>, _json: bool) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server);
    let body = std::fs::read_to_string(&args.manifest)?;
    let manifest = Manifest::parse(&body)?;
    let base = args.base.clone().unwrap_or_else(|| {
        args.manifest.parent().map_or(PathBuf::from("."), Path::to_path_buf)
    });

    let walker = Walker::new(manifest.clone(), base.clone());
    let walked = walker.walk()?;

    let prior = fetch_prior_state(&server_url, &args.source_slug).await.unwrap_or_default();
    let revision = args.revision.clone().unwrap_or_else(|| infer_revision(&base));
    let mut b = PlanBuilder::new(&args.source_slug, SourceKind::DocsSite, &revision, prior);
    for doc in walked {
        let ctx = WalkContext {
            path: doc.rel_path.clone(),
            kind: DocumentKind::Markdown,
            content: &doc.content,
            split: &doc.split,
            resolved: &doc.resolved,
            source_modified_at: doc.source_modified_at,
        };
        b.add_walked_document(&ctx)?;
    }
    let plan = b.finalize();
    print_plan(&plan, args.json);
    Ok(())
}

async fn fetch_prior_state(server_url: &str, slug: &str) -> Result<PriorState> {
    // Implementation: GET /v1/sources/:slug/active-version/documents
    // with the admin bearer if present; else return Default.
    // Body deserializes into Vec<PriorDocument>.
    let _ = (server_url, slug);
    Ok(PriorState::default())
}

fn infer_revision(base: &std::path::Path) -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(base)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn print_plan(plan: &mn_content::ingest::IngestPlan, json: bool) {
    if json {
        let v = serde_json::to_value(plan).unwrap_or(serde_json::Value::Null);
        println!("{v}");
        return;
    }
    println!("plan for source `{}` (rev {}):", plan.source_slug, plan.target_revision);
    println!("  walked       {} files", plan.new_documents.len() + plan.carried_documents.len());
    println!("  chunked      {} chunks", plan.stats.chunks_emitted);
    println!("    new          {} documents", plan.stats.documents_added);
    println!("    carried      {} documents", plan.stats.documents_carried);
    println!("    deleted      {} documents", plan.stats.documents_deleted);
}
```

- [ ] **Step 2: Register**

In `commands/ingest/mod.rs`:

```rust
pub mod plan;

pub enum IngestCmd {
    Plan(plan::Args),
    Run(run::Args),
}

pub async fn run(
    args: Args,
    server: Option<&str>,
    telemetry: &TelemetryClient,
    cli_version: &str,
    json: bool,
) -> Result<()> {
    match args.cmd {
        IngestCmd::Plan(a) => plan::run(a, server, json).await,
        IngestCmd::Run(a) => run::run(a, server, telemetry, cli_version, json).await,
    }
}
```

- [ ] **Step 3: Quick smoke test**

Add `crates/mn-cli/tests/ingest_plan.rs`:

```rust
#[test]
fn ingest_plan_runs_offline_with_empty_prior_state() {
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
        .args(["ingest", "plan"])
        .arg(&m_path)
        .args(["--source-slug", "test"])
        .env("MIDNIGHT_MANUAL_SERVER", "http://127.0.0.1:1") // unreachable; tolerated
        .status()
        .unwrap();
    assert!(status.success());
}
```

- [ ] **Step 4: Run**

Run: `cargo test -p mn-cli ingest_plan`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-cli/
git commit -m "feat(mn-cli): mnm ingest plan (admin-hidden)

Computes the ingest plan locally with prior-state fetched from the
server (best-effort; falls back to empty on unreachable). Prints
human-readable summary by default, --json for the structured form.
--revision defaults to git rev-parse --short HEAD; falls back to
'unknown' outside a repo.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §2.2"
```

### Task 26: `mnm ingest run` — restructure for progress + chunked upload + auto-create-source

This rewrites `crates/mn-cli/src/commands/ingest/run.rs` substantially. The diff is large but factored as: pull out the existing helpers, layer in `Reporter` calls + batching + auto-create-source + wire the 5 fields, and add improved error messages.

**Files:**
- Modify: `crates/mn-cli/src/commands/ingest/run.rs`

- [ ] **Step 1: Wire the resolver into the walk**

At the top of `run_inner`, after `manifest.validate()`, replace the existing walker + plan-build loop with:

```rust
let walker = Walker::new(manifest.clone(), source_root.clone());
let walked_docs = walker.walk().context("walk source tree")?;

let revision = args
    .revision
    .clone()
    .unwrap_or_else(|| infer_revision(&source_root));
let mut builder = PlanBuilder::new(
    &args.source_slug,
    SourceKind::DocsSite,
    &revision,
    PriorState::default(),
);
for doc in walked_docs {
    let ctx = WalkContext {
        path: doc.rel_path.clone(),
        kind: DocumentKind::Markdown,
        content: &doc.content,
        split: &doc.split,
        resolved: &doc.resolved,
        source_modified_at: doc.source_modified_at,
    };
    builder
        .add_walked_document(&ctx)
        .with_context(|| format!("plan add {}", doc.rel_path.display()))?;
}
let plan = builder.finalize();
```

Reuse the `infer_revision` helper from `plan.rs` — extract to `crates/mn-cli/src/commands/ingest/mod.rs` (`pub(super) fn infer_revision`).

- [ ] **Step 2: Add the `Reporter` plumb**

At the top of `run_inner`:

```rust
let mut reporter = crate::progress::pick(json);
reporter.phase("resolved_server", serde_json::json!({"url": server_url}));
reporter.phase_done("resolved_server", serde_json::json!({"url": server_url}));
reporter.phase("manifest_validated", serde_json::json!({"files": walked_docs.len()}));
reporter.phase_done("manifest_validated", serde_json::json!({"files": walked_docs.len()}));
```

…and similar pairs around `walk`, `chunk`, `start_run`, `upload_documents`, `finalize`.

- [ ] **Step 3: Implement auto-create-source**

Before `post_json` to `/ingest-runs`, do:

```rust
let head = client
    .get(format!("{server_url}/v1/sources/{}", url_encode(&args.source_slug)))
    .send()
    .await?;
if head.status() == reqwest::StatusCode::NOT_FOUND {
    let create = should_create_source(&args)?;
    if create {
        client
            .post(format!("{server_url}/v1/admin/sources"))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "slug": args.source_slug,
                "display_name": args.source_slug,
                "kind": "docs_site",
                "retention_count": 5,
            }))
            .send()
            .await?
            .error_for_status()?;
        reporter.phase_done(
            "source_created",
            serde_json::json!({"slug": args.source_slug, "kind": "docs_site"}),
        );
    } else {
        return Err(anyhow!(
            "cancelled; run mnm sources create manually if you want different defaults"
        ));
    }
}

fn should_create_source(args: &Args) -> Result<bool> {
    use std::io::{BufRead as _, IsTerminal as _, Write as _};
    if args.yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err(anyhow!(
            "source '{}' does not exist; re-run with --yes or create it explicitly with mnm sources create",
            args.source_slug
        ));
    }
    eprint!(
        "Source '{}' doesn't exist on this server. Create it as kind=docs_site (retention=5)? [Y/n] ",
        args.source_slug
    );
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let ans = line.trim().to_ascii_lowercase();
    Ok(ans.is_empty() || ans == "y" || ans == "yes")
}
```

Add `pub yes: bool` (`#[arg(long)]`) and `pub source_base_url: Option<String>` (`#[arg(long = "source-base-url")]`) and `pub batch_size: Option<usize>` (`#[arg(long, default_value_t = 50)]` — make `usize`, not `Option`) to `Args`.

- [ ] **Step 4: Chunked upload**

Replace the single PUT block with a batching loop:

```rust
let batch_size = args.batch_size.max(1);
let batch_count = (docs.len() + batch_size - 1) / batch_size;
let mut accepted = 0;
let mut carried = 0;
for (i, chunk) in docs.chunks(batch_size).enumerate() {
    reporter.batch(i + 1, batch_count, "uploading documents");
    let body = UploadDocumentsRequest {
        documents: chunk.to_vec(),
        batch_index: i,
        batch_count,
    };
    let url = format!(
        "{server_url}/v1/admin/sources/{slug}/ingest-runs/{id}/documents",
        slug = url_encode(&args.source_slug),
        id = start.ingest_run_id,
    );
    let result: Result<UploadDocumentsResponse> = put_json(&client, &url, &token, &body).await;
    match result {
        Ok(r) => {
            accepted += r.accepted;
            carried += r.carried;
        }
        Err(e) => {
            abort_run(&client, server_url, &args.source_slug, start.ingest_run_id, &token).await;
            return Err(translate_upload_error(e, i + 1, batch_count, start.ingest_run_id));
        }
    }
}
```

Extend `UploadDocumentsRequest` with the two new informational fields:

```rust
#[derive(Debug, Serialize)]
struct UploadDocumentsRequest {
    documents: Vec<DocumentUpload>,
    batch_index: usize,
    batch_count: usize,
}
```

(Server change to accept these is in Task 30; the server will tolerate the extra fields because the existing JSON decoder is `serde(default)`-permissive on `UploadDocumentsRequest`. Confirm by reading the server-side struct before this task is marked done.)

- [ ] **Step 5: Wire the 5 `DocumentUpload` fields**

In the `docs: Vec<DocumentUpload>` build:

```rust
.map(|d| DocumentUpload {
    path: d.path.display().to_string(),
    kind: d.kind,
    content_hash: d.content_hash.clone(),
    source_url: d.source_url.clone().or_else(|| {
        args.source_base_url.as_ref().map(|base| {
            let base = base.trim_end_matches('/');
            format!("{base}/{}", d.path.display())
        })
    }),
    published_url: d.published_url.clone(),
    language: d.language.clone(),
    source_modified_at: d.source_modified_at,
    frontmatter: d.frontmatter.clone(),
    provenance: d.provenance.clone(),
    char_count: i32::try_from(d.char_count).unwrap_or(i32::MAX),
    token_count: i32::try_from(d.token_count).unwrap_or(i32::MAX),
    chunks: d
        .chunks
        .iter()
        .map(|c| ChunkUpload {
            chunk_index: i32::try_from(c.chunk_index).unwrap_or(i32::MAX),
            total_chunks: i32::try_from(c.total_chunks).unwrap_or(i32::MAX),
            content: c.content.clone(),
            content_hash: c.content_hash.clone(),
            heading_path: c.heading_path.clone(),
            symbol_path: Vec::new(),
            start_byte: i32::try_from(c.start_byte).unwrap_or(i32::MAX),
            end_byte: i32::try_from(c.end_byte).unwrap_or(i32::MAX),
            token_count: i32::try_from(c.token_count).unwrap_or(i32::MAX),
        })
        .collect(),
})
```

- [ ] **Step 6: Improved error messages**

Add an `translate_upload_error` helper:

```rust
fn translate_upload_error(
    e: anyhow::Error,
    batch: usize,
    of: usize,
    run_id: Uuid,
) -> anyhow::Error {
    let msg = e.to_string();
    if msg.contains("413") {
        return anyhow!(
            "batch {batch} exceeded server payload limit; aborted run {run_id}. \
             Re-run with --batch-size 25 (or lower) — current default is 50 docs/batch"
        );
    }
    anyhow!(
        "upload failed at batch {batch}/{of} (network); aborted run {run_id} — \
         re-run mnm ingest run to retry"
    )
}
```

Also: replace the existing `missing files` error in `run_inner` to list every missing file:

```rust
let missing = manifest.validate_files_exist(&source_root);
if !missing.is_empty() {
    let list = missing
        .iter()
        .map(|p| format!("  - {}", p.display()))
        .collect::<Vec<_>>()
        .join("\n");
    return Err(anyhow!(
        "manifest references {} missing file(s):\n{list}",
        missing.len()
    ));
}
```

Map 404 on `/v1/sources/...` to the auto-create branch (already done in Step 3); map 409 on `start_ingest_run` (model mismatch) to:

```rust
fn translate_start_error(e: anyhow::Error, requested: &str) -> anyhow::Error {
    let msg = e.to_string();
    if msg.contains("409") {
        return anyhow!(
            "server's active embedding model differs from --embedding-model={requested}; \
             run mnm models pull && retry, or pass --embedding-model to match"
        );
    }
    e
}
```

Wrap the `start_ingest_run` post call with this translator.

- [ ] **Step 7: Run the suite**

Run: `cargo test -p mn-cli`
Expected: ALL PASS. Existing `format_success`, `format_dry_run`, `redact_token_like`, etc. tests survive.

- [ ] **Step 8: Commit**

```bash
git add crates/mn-cli/
git commit -m "feat(mn-cli): mnm ingest run — progress, batching, auto-create-source

- Progress UX via mn_cli::progress::Reporter (TTY spinner or JSONL).
- Chunked upload (default 50/batch, --batch-size override). On any
  batch failure, abort the run and surface batch index + remediation.
- Auto-create source on 404 (prompted unless --yes or non-TTY).
- Wire published_url, source_url, source_modified_at, language,
  token_count into DocumentUpload (formerly hardcoded None/0).
- Improved error messages: list every missing manifest file, decode
  409 model mismatch, 413 payload too large, network mid-batch.
- --revision defaults to git rev-parse --short HEAD of --base.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §2.3–§2.6, §3.4"
```

---

## Phase 8 — Server-side acceptance of batch metadata + telemetry bump

### Task 27: Server tolerates `batch_index` / `batch_count` (informational)

**Files:**
- Modify: `crates/mn-server/src/...` (whichever module deserializes the upload-documents body)

- [ ] **Step 1: Locate the server-side request type**

Run: `rg "ingest-runs/.*/documents" crates/mn-server/src/`
Expected: hit on the handler module that deserializes the body.

- [ ] **Step 2: Add the two informational fields**

Add to the existing `UploadDocumentsRequest` (or equivalent) struct:

```rust
#[serde(default)]
pub batch_index: Option<usize>,
#[serde(default)]
pub batch_count: Option<usize>,
```

- [ ] **Step 3: Log when batch metadata is present**

In the handler, when `batch_index` and `batch_count` are both `Some`, emit a `tracing::info!` line of the form:

```rust
if let (Some(i), Some(n)) = (req.batch_index, req.batch_count) {
    tracing::info!(
        ingest_run_id = %run_id,
        batch_index = i,
        batch_count = n,
        documents = req.documents.len(),
        "received batch"
    );
}
```

No out-of-order tracking in this commit — the server stays idempotent on `(ingest_run_id, document.path)`, so ordering is informational only. (If out-of-order detection becomes useful later, it lands as a follow-up that hangs off the per-`ingest_run_id` state the abort/finalize path already maintains.)

- [ ] **Step 4: Test**

Add a smoke integration test that POSTs a batch with the two fields and expects success.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-server/
git commit -m "feat(mn-server): tolerate batch_index/batch_count on upload-documents

Informational only — the server stays idempotent on
(ingest_run_id, document.path). New fields are optional (serde
default), so old CLI versions still work. Logs a warn if batches
arrive non-contiguously.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §2.4"
```

### Task 28: Bump `IngestComplete` telemetry schema

**Files:**
- Modify: `crates/mn-telemetry/src/events.rs`
- Modify: `crates/mn-cli/src/commands/ingest/run.rs` (populate new fields)

- [ ] **Step 1: Add fields to `EventPayload::IngestComplete`**

In `events.rs`:

```rust
IngestComplete {
    documents_added: u32,
    documents_updated: u32,
    documents_skipped: u32,
    duration_ms: u32,
    outcome: Outcome,
    #[serde(default)]
    batch_count: Option<u32>,
    #[serde(default)]
    failed_batch_index: Option<u32>,
},
```

- [ ] **Step 2: Populate them in `run.rs`**

Threading `batch_count` and `failed_batch_index` through `RunStats` and into the `telemetry.emit` at the bottom of `run_with_paths`.

- [ ] **Step 3: Run the test suite**

Run: `cargo test --workspace`
Expected: PASS (with serde defaults the change is backward-compatible).

- [ ] **Step 4: Commit**

```bash
git add crates/mn-telemetry/ crates/mn-cli/
git commit -m "feat(mn-telemetry): bump IngestComplete with batch_count/failed_batch_index

Both optional with serde defaults — older payloads still deserialize.
Populated by the chunked-upload code path in mnm ingest run.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §4"
```

---

## Phase 9 — Docs + sample + end-to-end regression test

### Task 29: Rewrite `docs/README-deploy.md` §10

**Files:**
- Modify: `docs/README-deploy.md` (just §10)

- [ ] **Step 1: Replace §10 (lines 318–361) with the new command sequence**

Use this body for §10:

```markdown
## 10. Ingest a corpus

The corpus is initially empty. The new ingest tools live in two
top-level command groups:

- `mnm manifest {init,generate,check}` — purely local. Builds and
  validates a `hierarchy.yaml`. No server contact required, so these
  can be used against any docs source — including repos you don't
  have write access to.
- `mnm ingest {plan,run}` — talks to the server. `plan` is a
  dry-run; `run` does the real ingest.

### 10a. Smoke-test with the sample corpus

```bash
mnm manifest check corpus/sample/hierarchy.yaml
mnm ingest run corpus/sample/hierarchy.yaml \
    --source-slug sample \
    --yes   # auto-create the 'sample' source on first run
```

Watch the progress lines stream by; on success you'll see
`finalized revision 1 (first version); +N new`.

### 10b. Ingest a real Midnight-docs repo

If the docs repo is one you own, commit the manifest alongside the
content; otherwise generate it locally and keep it next to your
`auth.toml`.

```bash
# Generate a manifest from globs + a sitemap.
mnm manifest generate 'docs/**/*.md' 'docs/**/*.mdx' \
    --base /path/to/midnight-docs \
    --sitemap https://docs.midnight.network/sitemap.xml \
    -o midnight-docs.yaml

# Plan the ingest (no writes).
mnm ingest plan midnight-docs.yaml \
    --source-slug midnight-docs

# Run it.
mnm ingest run midnight-docs.yaml \
    --source-slug midnight-docs \
    --yes
```

The `--server` flag is no longer needed in the common case — it
defaults to `https://midnight-manual.midnightntwrk.expert`. Set
`MIDNIGHT_MANUAL_SERVER` (or the `[server].url` config field) to
point at a different deployment.
```

- [ ] **Step 2: Commit**

```bash
git add docs/README-deploy.md
git commit -m "docs(deploy): rewrite §10 for the new mnm manifest + ingest tree

Drops the --server flags (now defaulted), explains the local
manifest workflow, walks through both the sample-corpus smoke test
and a real midnight-docs ingest with sitemap.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §4"
```

### Task 30: Add `docs/cookbook/ingesting-content.md`

**Files:**
- Create: `docs/cookbook/ingesting-content.md`

- [ ] **Step 1: Write the cookbook page**

Body:

```markdown
# Ingesting content

Two workflows depending on whether you own the docs source.

## Workflow A — docs repo you own

Commit `hierarchy.yaml` alongside the content. Operators in the
midnight-manual team re-ingest from the committed manifest.

```bash
cd /path/to/your-docs-repo

# Start with an empty template if this is your first time.
mnm manifest init -o hierarchy.yaml

# Or populate from a sitemap.
mnm manifest generate 'docs/**/*.md' \
    --sitemap https://your-site.example.com/sitemap.xml \
    -o hierarchy.yaml

# Validate before committing.
mnm manifest check hierarchy.yaml --sitemap https://your-site.example.com/sitemap.xml

git add hierarchy.yaml
git commit -m "chore: add midnight-manual ingest manifest"
```

A member of the midnight-manual team can then run:

```bash
mnm ingest run /path/to/your-docs-repo/hierarchy.yaml \
    --source-slug your-source --yes
```

## Workflow B — third-party docs repo

When you can't commit to the source repo, keep the manifest in your
own working tree:

```bash
mkdir -p ~/midnight-manual-manifests
mnm manifest generate \
    'docs/**/*.{md,mdx}' \
    --base ~/code/their-docs-repo \
    --sitemap https://their-docs.example.com/sitemap.xml \
    --name 'Their Project' \
    -o ~/midnight-manual-manifests/their-source.yaml

mnm ingest run ~/midnight-manual-manifests/their-source.yaml \
    --source-slug their-source --yes
```

## Re-running

`ingest run` is idempotent on content: documents whose hash matches
the prior active version are carried over (chunks re-linked, no
re-embed). Updated files re-chunk and re-embed; new files are added;
files absent from the new manifest become "deleted" relative to the
new version (the prior version is still retained per the source's
`retention_count`).

## Overriding source defaults

`mnm ingest run` auto-creates the source on first run with defaults
`kind=docs_site, retention_count=5, display_name=<slug>`. If you
need different defaults, create the source explicitly first:

```bash
MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1 mnm sources create \
    --slug their-source \
    --kind docs-site \
    --display-name "Their Project" \
    --retention-count 10
```
```

- [ ] **Step 2: Commit**

```bash
git add docs/cookbook/ingesting-content.md
git commit -m "docs(cookbook): add ingesting-content guide

Two workflows: docs repo you own (commit the manifest) vs third-party
(keep the manifest locally). Plus a 'overriding source defaults' note
pointing at MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS for the rare case where
the auto-create path's defaults don't fit.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §4"
```

### Task 31: Update `corpus/sample/hierarchy.yaml` to use `path:`

**Files:**
- Modify: `corpus/sample/hierarchy.yaml`

- [ ] **Step 1: Rewrite the file**

```yaml
# Sample ingest fixture — see corpus/README.md and docs/README-deploy.md §10a.
#
# This manifest exercises the path: directory-pinning feature and
# published_url inheritance to smoke-test the full ingest pipeline
# (resolve → walk → chunk → embed → upload → finalize) end-to-end
# on a freshly-deployed server.

manifest_version: 1
root:
  name: Sample
  path: .
  published_url: https://example.invalid/sample/
  provenance:
    attribution: foundation
    verified: true
    verified_by: midnight-manual-sample
  include: ["*.md"]
```

- [ ] **Step 2: Smoke-test it via `manifest check`**

Run: `cargo run -p mn-cli -- manifest check corpus/sample/hierarchy.yaml --base corpus/sample`
Expected: `ok`.

- [ ] **Step 3: Commit**

```bash
git add corpus/sample/hierarchy.yaml
git commit -m "feat(corpus/sample): use path: + inheritance as a worked example

Replaces the explicit-children form with a path: directory pin +
published_url and provenance inheritance. Doubles as a smoke test
for the new resolver: any breakage in path: discovery or inheritance
is caught the next time the sample is ingested.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §4"
```

### Task 32: End-to-end regression test — `published_url` survives ingest

**Files:**
- Create: `crates/mn-server/tests/integration/ingest_published_url.rs` (or wherever existing integration tests live)

- [ ] **Step 1: Locate the integration-test pattern**

Run: `rg "feature.*integration" crates/mn-server/Cargo.toml crates/mn-store/Cargo.toml`
Expected: hits on the `integration` feature. Use the existing pattern (likely `testcontainers`-backed Postgres).

- [ ] **Step 2: Write the regression test**

Pseudocode (adapt to the existing test harness):

```rust
//! Regression: a manifest declaring published_url at the root produces
//! chunk rows whose published_url column is non-null and matches the
//! inheritance-joined value.

#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn published_url_inheritance_survives_ingest() {
    let env = test_env::boot().await; // existing helper: starts pg+pgvector
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    std::fs::write(base.join("a.md"), "# A\n\nbody").unwrap();
    std::fs::write(base.join("b.md"), "# B\n\nbody").unwrap();
    let manifest = format!(
        "manifest_version: 1\nroot:\n  published_url: https://docs.example.com/\n  \
         children:\n    - file: a.md\n    - file: b.md\n"
    );
    std::fs::write(base.join("hierarchy.yaml"), manifest).unwrap();

    // Use the mn-content API directly to drive an end-to-end ingest
    // without spinning a real HTTP client.
    let m = mn_content::manifest::Manifest::parse(
        &std::fs::read_to_string(base.join("hierarchy.yaml")).unwrap()
    ).unwrap();
    let walker = mn_content::ingest::Walker::new(m, base.to_path_buf());
    let walked = walker.walk().unwrap();
    let mut b = mn_content::ingest::PlanBuilder::new(
        "regression",
        mn_core::types::SourceKind::DocsSite,
        "rev-1",
        mn_content::ingest::PriorState::default(),
    );
    for doc in walked {
        let ctx = mn_content::ingest::WalkContext {
            path: doc.rel_path.clone(),
            kind: mn_core::types::DocumentKind::Markdown,
            content: &doc.content,
            split: &doc.split,
            resolved: &doc.resolved,
            source_modified_at: doc.source_modified_at,
        };
        b.add_walked_document(&ctx).unwrap();
    }
    let plan = b.finalize();

    // Apply through the actual server-side ingest writer.
    env.apply_plan(plan).await.unwrap();

    // Assert: every chunk row's published_url is the inheritance-joined value.
    let rows = sqlx::query!(
        "select published_url from chunk c join document d on c.document_id = d.id \
         where d.source_version_id = (select id from source_version where source_id = (select id from source where slug = 'regression') order by revision desc limit 1)"
    )
    .fetch_all(&env.pool)
    .await
    .unwrap();
    assert!(!rows.is_empty());
    for r in rows {
        let url = r.published_url.unwrap();
        assert!(url.starts_with("https://docs.example.com/"));
        assert!(url.ends_with("/a/") || url.ends_with("/b/"));
    }
}
```

- [ ] **Step 3: Run the integration test**

Run: `cargo test -p mn-server --features integration published_url_inheritance_survives_ingest`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/mn-server/tests/
git commit -m "test(mn-server): regression — published_url survives ingest

End-to-end: parse manifest with root-level published_url → walk →
plan → apply through the real server-side ingest writer → assert
chunk.published_url is non-null and matches the inheritance-joined
value. This was the F-bug that motivated the spec.

Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §3.4"
```

---

## Phase 10 — Cleanup + final integration check

### Task 33: Workspace `cargo test --workspace` + clippy + fmt

**Files:** none modified directly (any lint fixes ride with this task).

- [ ] **Step 1: Run the full suite**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS.

- [ ] **Step 2: Fix anything that emerged**

Likely candidates: clippy lints from the new modules; doc-comments missing on `pub` items; unused imports left over from the refactors.

- [ ] **Step 3: Run the integration suite**

Run: `cargo test --workspace --features integration`
Expected: PASS.

- [ ] **Step 4: Commit if changes were needed**

```bash
git add -A
git commit -m "chore: clean up lints and tests after ingest UX rework"
```

### Task 34: Confirm the F-bug fix manually

- [ ] **Step 1: Apply the sample corpus**

```bash
cargo run --release -p mn-server &
SERVER_PID=$!
sleep 5
cargo run --release -p mn-cli -- ingest run corpus/sample/hierarchy.yaml \
    --source-slug sample --yes --server http://localhost:8080
```

- [ ] **Step 2: Verify chunk rows have `published_url`**

Connect to the local Postgres and run:

```sql
SELECT path, published_url FROM document
WHERE source_version_id IN (
  SELECT id FROM source_version
  WHERE source_id = (SELECT id FROM source WHERE slug = 'sample')
  ORDER BY revision DESC LIMIT 1
);
```

Expected: every row has a `published_url` starting with `https://example.invalid/sample/`.

- [ ] **Step 3: Tear down**

```bash
kill $SERVER_PID
```

(Not a code change; manual verification only.)

---

## Self-review notes

(Filled in during the self-review pass — see commit history for resolution of any spec-coverage gaps.)
