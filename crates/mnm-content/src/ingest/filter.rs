//! `ignore`-based file filter with a deterministic precedence ladder.
//!
//! [`FileFilter`] combines four orthogonal filtering mechanisms into one
//! decision function ([`FileFilter::allows`]) and a real filesystem walk
//! ([`FileFilter::walk`]) built on the [`ignore`] crate.
//!
//! # Precedence order (first match wins)
//!
//! 1. **`.git/` component** — always excluded, not disableable.
//! 2. **Default skip list** (when [`FilterOptions::default_ignore_list`] is
//!    `true`) — directory components `node_modules`, `target`, `vendor`,
//!    `dist`, `build`, `out`, `coverage`, `managed`, `__snapshots__`;
//!    filename globs `*.min.js`, `*.bundle.js`, `*_pb.ts`, `*_pb.js`,
//!    `*.pb.go`, `package-lock.json`, `npm-shrinkwrap.json`,
//!    `pnpm-lock.yaml`, `CODE_OF_CONDUCT.md`, `CONTRIBUTING.md`,
//!    `SECURITY.md`.
//! 3. *(gitignore layer)* — applied by `walk` via the `ignore` crate; not
//!    consulted by the pure [`FileFilter::allows`] function.
//! 4. **Excludes** — any path matching one of [`FilterOptions::excludes`] is
//!    excluded (exclude beats include).
//! 5. **Includes whitelist** — when [`FilterOptions::includes`] is non-empty,
//!    a path must match at least one include glob or it is excluded.
//!    5b. **Known-kind gate** — when `require_known_kind` is `true` and the
//!    include whitelist is empty, a file whose extension is not a recognised
//!    language is excluded (an explicit include bypasses this).
//! 6. **Default** — included.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};

// ---------------------------------------------------------------------------
// License file detection
// ---------------------------------------------------------------------------

/// True for LICENSE-family filenames.
///
/// Matches stems {LICENSE, LICENCE, COPYING, NOTICE, PATENTS} (case-insensitive),
/// alone or followed by `-`/`.` + suffix (`LICENSE-MIT`, `COPYING.lesser`).
/// Does not match `licensed-features.md` or `notices.md` where the stem does
/// not end exactly at the separator.
#[must_use]
pub fn is_license_filename(basename: &str) -> bool {
    const STEMS: &[&str] = &["LICENSE", "LICENCE", "COPYING", "NOTICE", "PATENTS"];
    let upper = basename.to_ascii_uppercase();
    STEMS.iter().any(|stem| {
        upper == *stem
            || (upper.starts_with(stem)
                && matches!(upper.as_bytes().get(stem.len()), Some(b'-' | b'.')))
    })
}

// ---------------------------------------------------------------------------
// FilterOptions
// ---------------------------------------------------------------------------

/// Configuration for [`FileFilter`].
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)] // Four independent feature-gate flags; a state machine would be less clear.
pub struct FilterOptions {
    /// Glob patterns for paths that should be included.
    ///
    /// When this list is non-empty it acts as a whitelist: a path must match
    /// at least one pattern, otherwise it is excluded (after higher-priority
    /// rules have been applied). When empty every path is tentatively allowed.
    pub includes: Vec<String>,

    /// Glob patterns for paths that should be excluded.
    ///
    /// Exclusions are evaluated before the include whitelist, so an exclude
    /// pattern that overlaps with an include pattern will win.
    pub excludes: Vec<String>,

    /// Whether to honor `.gitignore`, `.git/info/exclude`, and parent
    /// directory ignore files during [`FileFilter::walk`].
    ///
    /// Has no effect on the pure [`FileFilter::allows`] function.
    pub respect_gitignore: bool,

    /// Apply the built-in default skip-list (see the module-level docs /
    /// `DEFAULT_SKIP_COMPONENTS` + the default filename globs).
    pub default_ignore_list: bool,

    /// Skip dotfiles and dot-directories (e.g. `.env`, `.github/`).
    /// `true` for ingest; `false` for generate (which relies on gitignore).
    pub skip_hidden: bool,

    /// Drop files whose extension is not a recognised language, UNLESS an
    /// `include` glob matches. `true` for ingest; `false` for generate.
    pub require_known_kind: bool,
}

// ---------------------------------------------------------------------------
// FileFilter
// ---------------------------------------------------------------------------

/// Directory components that are always skipped when
/// [`FilterOptions::default_ignore_list`] is `true`.
static DEFAULT_SKIP_COMPONENTS: &[&str] = &[
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    "out",
    "coverage",
    "managed",
    "__snapshots__",
];

/// A compiled file filter ready for repeated use.
///
/// Construct with [`FileFilter::new`] and query with [`FileFilter::allows`] or
/// [`FileFilter::walk`].
pub struct FileFilter {
    /// Original options, stored so `walk` can honour `respect_gitignore`.
    opts: FilterOptions,
    /// Compiled include globs (empty ⇒ unconditional allow at step 5).
    include_set: GlobSet,
    /// Compiled exclude globs.
    exclude_set: GlobSet,
    /// Compiled default-skip filename globs (`*.min.js` etc.).
    ///
    /// Directory-component skips are handled by splitting on `/` instead of
    /// via glob matching.
    default_file_set: GlobSet,
}

impl FileFilter {
    /// Build a [`FileFilter`] from the provided options.
    ///
    /// # Panics
    ///
    /// Panics if any glob pattern in `opts.includes` or `opts.excludes` is
    /// syntactically invalid. Use validated inputs (e.g. from CLI arg
    /// parsing) to avoid this.
    #[must_use]
    pub fn new(opts: FilterOptions) -> Self {
        // Glob-matching strategy
        // ----------------------
        // `globset::Glob` compiles patterns into regexes anchored with `^`
        // and `$`.  When `literal_separator = false` (the default), `*`
        // compiles to `.*`, so it can span `/`.  However, a pattern like
        // `generated_*.rs` compiles to `^generated_.*\.rs$`, which only
        // matches paths that START with `generated_` — it would not match
        // `src/generated_x.rs`.
        //
        // To work around this, `glob_matches` tests a glob set against BOTH
        // the full relative path and the file's basename (everything after
        // the last `/`).  This means:
        // - `*.rs`          → matches `lib.rs` (basename) and `src/lib.rs` (full)
        // - `generated_*.rs`→ matches `generated_x.rs` (basename)
        // - `src/*.rs`      → matches `src/lib.rs` (full path)
        //
        // For the default-skip filename patterns (`*.min.js` etc.) the same
        // dual approach catches files at any depth.
        //
        // The directory-component skips (`node_modules`, `target`, …) are
        // implemented by splitting the path on `/` and comparing components
        // directly — simpler and more robust than glob for that case.
        let include_set = Self::build_set(&opts.includes);
        let exclude_set = Self::build_set(&opts.excludes);
        let default_file_set = Self::build_set(&[
            "*.min.js".to_owned(),
            "*.bundle.js".to_owned(),
            "*_pb.ts".to_owned(),
            "*_pb.js".to_owned(),
            "*.pb.go".to_owned(),
            "package-lock.json".to_owned(),
            "npm-shrinkwrap.json".to_owned(),
            "pnpm-lock.yaml".to_owned(),
            "CODE_OF_CONDUCT.md".to_owned(),
            "CONTRIBUTING.md".to_owned(),
            "SECURITY.md".to_owned(),
        ]);
        Self {
            opts,
            include_set,
            exclude_set,
            default_file_set,
        }
    }

    /// Build a [`GlobSet`] from a slice of pattern strings.
    ///
    /// # Panics
    ///
    /// Panics on an invalid pattern.
    fn build_set(patterns: &[String]) -> GlobSet {
        let mut builder = GlobSetBuilder::new();
        for pat in patterns {
            let glob = Glob::new(pat).unwrap_or_else(|e| panic!("invalid glob `{pat}`: {e}"));
            builder.add(glob);
        }
        builder
            .build()
            .expect("GlobSetBuilder::build should not fail for valid globs")
    }

    /// Return `true` if `set` matches `rel_path` OR the basename of
    /// `rel_path`.
    ///
    /// `globset` anchors regex patterns to the start of the path, so a
    /// pattern like `generated_*.rs` won't match `src/generated_x.rs` when
    /// tested against the full path.  Testing against the basename as well
    /// ensures user-supplied `*.ext` and `prefix_*.ext` patterns work
    /// intuitively regardless of directory depth.
    fn glob_matches(set: &GlobSet, rel_path: &str) -> bool {
        if set.is_match(rel_path) {
            return true;
        }
        // Extract basename: everything after the last '/'.
        let basename = rel_path.rsplit('/').next().unwrap_or(rel_path);
        set.is_match(basename)
    }

    /// Decide whether `rel_path` is allowed by this filter.
    ///
    /// `rel_path` must use `/` as the path separator (i.e. it should be a
    /// repo-relative POSIX-style path).  This function does **not** touch the
    /// filesystem; gitignore rules (if enabled) are applied separately in
    /// [`FileFilter::walk`].
    ///
    /// The evaluation order follows the precedence ladder documented in the
    /// module-level docs.
    #[must_use]
    pub fn allows(&self, rel_path: &str) -> bool {
        // ----------------------------------------------------------------
        // Step 1 — `.git/` component: always excluded.
        // ----------------------------------------------------------------
        if rel_path.split('/').any(|c| c == ".git") {
            return false;
        }

        // ----------------------------------------------------------------
        // Step 2 — Default skip list.
        // ----------------------------------------------------------------
        if self.opts.default_ignore_list {
            // 2a. Directory-component check (split on '/' for robustness).
            for component in rel_path.split('/') {
                if DEFAULT_SKIP_COMPONENTS.contains(&component) {
                    return false;
                }
            }

            // 2b. Filename glob check (`*.min.js` etc.).
            if Self::glob_matches(&self.default_file_set, rel_path) {
                return false;
            }

            // 2c. License-family files by stem (spec §License files stop
            // being documents): LICENSE / LICENCE / COPYING / NOTICE /
            // PATENTS, optionally followed by `-` or `.` + anything.
            let basename = rel_path.rsplit('/').next().unwrap_or(rel_path);
            if is_license_filename(basename) {
                // Discovery-time exclusions are otherwise silent; trace this one
                // so the rare false positive (e.g. `NOTICE-BOARD.md`) is at least
                // greppable rather than vanishing without a record.
                tracing::debug!(path = rel_path, "excluding LICENSE-family file from ingest");
                return false;
            }
        }

        // ----------------------------------------------------------------
        // Step 3 — gitignore (applied by `walk`; no-op here).
        // ----------------------------------------------------------------

        // ----------------------------------------------------------------
        // Step 4 — Excludes.
        // ----------------------------------------------------------------
        if !self.opts.excludes.is_empty() && Self::glob_matches(&self.exclude_set, rel_path) {
            return false;
        }

        // ----------------------------------------------------------------
        // Step 5 — Include whitelist.
        // ----------------------------------------------------------------
        if !self.opts.includes.is_empty() && !Self::glob_matches(&self.include_set, rel_path) {
            return false;
        }

        // ----------------------------------------------------------------
        // Step 5b — Known-kind gate (ingest). Only when no include whitelist
        // is in effect: an explicit include is itself the intent signal.
        // ----------------------------------------------------------------
        if self.opts.require_known_kind
            && self.opts.includes.is_empty()
            && crate::language::from_path(std::path::Path::new(rel_path)).is_none()
        {
            return false;
        }

        // ----------------------------------------------------------------
        // Step 6 — Default: included.
        // ----------------------------------------------------------------
        true
    }

    /// Build the `ignore` walker for `root` with this filter's hidden/gitignore/
    /// pruning settings applied.
    fn configure_builder(&self, root: &Path) -> ignore::WalkBuilder {
        let mut builder = ignore::WalkBuilder::new(root);
        // Honour the caller's hidden-file preference.
        builder.hidden(self.opts.skip_hidden);
        // Wire gitignore layers to the `respect_gitignore` option.
        builder.git_ignore(self.opts.respect_gitignore);
        builder.git_exclude(self.opts.respect_gitignore);
        builder.ignore(self.opts.respect_gitignore);
        // Determinism: only the in-tree .gitignore counts — never the machine
        // global (`core.excludesFile`) or parent-directory ignore files.
        builder.git_global(false);
        builder.parents(false);
        // Apply .gitignore rules even when no `.git` directory is present
        // (e.g. in subdirectories or temporary trees used during testing).
        builder.require_git(false);
        // Prune noise directories so we never descend into them (perf + parity
        // with the old walkdir pruning). `.git` is always pruned; the rest only
        // when the default ignore list is active. Never prune the walk root
        // itself (depth 0).
        let prune_defaults = self.opts.default_ignore_list;
        builder.filter_entry(move |e| {
            if e.depth() == 0 {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            if name == ".git" {
                return false;
            }
            if prune_defaults
                && e.file_type().is_some_and(|ft| ft.is_dir())
                && DEFAULT_SKIP_COMPONENTS.contains(&name.as_ref())
            {
                return false;
            }
            true
        });
        builder
    }

    /// Enumerate all files under `root` that pass this filter.
    ///
    /// The walk is built on [`ignore::WalkBuilder`] which handles gitignore
    /// files, `.git/info/exclude`, and parent-directory ignore files when
    /// [`FilterOptions::respect_gitignore`] is `true`.
    ///
    /// On top of the `ignore`-crate layer, every candidate path is also
    /// tested against [`FileFilter::allows`] so that the `.git`-component,
    /// default-skip-list, exclude, and include rules are enforced.
    ///
    /// Returns a `Vec<PathBuf>` of **absolute** paths rather than
    /// `impl Iterator<Item = PathBuf>` to avoid lifetime complications with
    /// the closure capturing `self`.
    #[must_use]
    pub fn walk(&self, root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in self.configure_builder(root).build() {
            let Ok(entry) = entry else { continue };
            // Skip directories; we only care about files.
            if entry.file_type().is_none_or(|ft| !ft.is_file()) {
                continue;
            }
            let path = entry.path();
            // Compute the path relative to `root` and convert to a
            // `/`-separated string for `allows`.
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let rel_str = rel
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            if self.allows(&rel_str) {
                out.push(path.to_path_buf());
            }
        }
        out
    }

    /// Enumerate files under `base/subdir`, returning paths **relative to
    /// `base`** (so manifest `include`/`exclude` globs keep their
    /// source-root-relative meaning).
    ///
    /// The supplied walk root (depth 0) is not subject to the hidden-file
    /// filter, so a `path:` pointing at a dot-directory would be descended
    /// into.
    #[must_use]
    pub fn walk_subtree(&self, base: &Path, subdir: &Path) -> Vec<PathBuf> {
        let walk_root = base.join(subdir);
        let mut out = Vec::new();
        for entry in self.configure_builder(&walk_root).build() {
            let Ok(entry) = entry else { continue };
            if entry.file_type().is_none_or(|ft| !ft.is_file()) {
                continue;
            }
            // Strip `base` (not `walk_root`) so the returned path is
            // base-relative (e.g. `prerelease/top.md`), matching the
            // source-root-relative semantics expected by include/exclude globs.
            let Ok(rel) = entry.path().strip_prefix(base) else {
                continue;
            };
            let rel_str = rel
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            if self.allows(&rel_str) {
                out.push(PathBuf::from(rel_str));
            }
        }
        out.sort();
        out
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_ladder() {
        let f = FileFilter::new(FilterOptions {
            includes: vec!["*.rs".into()],
            excludes: vec!["generated_*.rs".into()],
            respect_gitignore: true,
            default_ignore_list: true,
            skip_hidden: true,
            require_known_kind: false,
        });
        assert!(f.allows("src/lib.rs"));
        assert!(!f.allows("src/generated_x.rs")); // exclude beats include
        assert!(!f.allows("src/main.ts")); // not in whitelist
        assert!(!f.allows("node_modules/pkg/index.rs")); // default skip
        assert!(!f.allows(".git/config")); // always
    }

    #[test]
    fn disable_default_list_allows_node_modules() {
        let f = FileFilter::new(FilterOptions {
            includes: vec![],
            excludes: vec![],
            respect_gitignore: false,
            default_ignore_list: false,
            skip_hidden: true,
            require_known_kind: false,
        });
        assert!(f.allows("node_modules/pkg/x.rs"));
        assert!(!f.allows(".git/config")); // .git still excluded
    }

    #[test]
    fn default_skip_list_covers_noise_dirs_files_and_boilerplate() {
        let f = FileFilter::new(FilterOptions {
            includes: vec![],
            excludes: vec![],
            respect_gitignore: false,
            default_ignore_list: true,
            skip_hidden: true,
            require_known_kind: false,
        });
        // New noise dirs
        for p in [
            "vendor/x.rs",
            "build/x.js",
            "out/x.js",
            "coverage/x.js",
            "managed/Contract.ts",
            "pkg/__snapshots__/a.snap.ts",
        ] {
            assert!(!f.allows(p), "{p} should be skipped");
        }
        // Lockfiles that masquerade as known kinds
        for p in [
            "package-lock.json",
            "a/npm-shrinkwrap.json",
            "b/pnpm-lock.yaml",
        ] {
            assert!(!f.allows(p), "{p} should be skipped");
        }
        // Generated
        for p in ["api_pb.ts", "api_pb.js", "api.pb.go"] {
            assert!(!f.allows(p), "{p} should be skipped");
        }
        // Boilerplate docs at any depth
        for p in ["CODE_OF_CONDUCT.md", "CONTRIBUTING.md", "sub/SECURITY.md"] {
            assert!(!f.allows(p), "{p} should be skipped");
        }
        // Kept (signal)
        for p in [
            "package.json",
            "tsconfig.json",
            "Cargo.toml",
            "src/lib.rs",
            "README.md",
        ] {
            assert!(f.allows(p), "{p} should be kept");
        }
    }

    #[test]
    fn gitignore_is_repo_local_not_parent_or_global() {
        let outer = tempfile::tempdir().unwrap();
        // Parent-level .gitignore that would (wrongly) hide parent_ignored.rs.
        std::fs::write(outer.path().join(".gitignore"), "parent_ignored.rs\n").unwrap();
        let repo = outer.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join(".gitignore"), "repo_ignored.rs\n").unwrap();
        std::fs::write(repo.join("keep.rs"), "x").unwrap();
        std::fs::write(repo.join("repo_ignored.rs"), "x").unwrap();
        std::fs::write(repo.join("parent_ignored.rs"), "x").unwrap();

        let f = FileFilter::new(FilterOptions {
            includes: vec![],
            excludes: vec![],
            respect_gitignore: true,
            default_ignore_list: true,
            skip_hidden: true,
            require_known_kind: false,
        });
        let mut got: Vec<String> = f
            .walk(&repo)
            .iter()
            .map(|p| {
                p.strip_prefix(&repo)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        got.sort();
        // repo's own .gitignore is honoured; the PARENT's is not.
        assert_eq!(got, vec!["keep.rs", "parent_ignored.rs"]);
    }

    #[test]
    fn require_known_kind_drops_unknown_unless_included() {
        let base = FilterOptions {
            includes: vec![],
            excludes: vec![],
            respect_gitignore: false,
            default_ignore_list: true,
            skip_hidden: true,
            require_known_kind: true,
        };
        let f = FileFilter::new(base.clone());
        assert!(f.allows("src/lib.rs"), "known kind kept");
        assert!(f.allows("data.json"), "json is a known kind, kept");
        assert!(!f.allows("notes.weirdext"), "unknown kind dropped");

        // Explicit include bypasses the gate.
        let f2 = FileFilter::new(FilterOptions {
            includes: vec!["**/*.weirdext".into()],
            ..base.clone()
        });
        assert!(f2.allows("notes.weirdext"), "include bypasses known-kind gate");

        // Disabled gate keeps unknown.
        let f3 = FileFilter::new(FilterOptions {
            require_known_kind: false,
            ..base
        });
        assert!(f3.allows("notes.weirdext"), "gate off keeps unknown");
    }

    #[test]
    fn walk_honours_skip_hidden_and_prunes_node_modules() {
        let dir = tempfile::tempdir().unwrap();
        let b = dir.path();
        std::fs::write(b.join("keep.rs"), "fn a(){}").unwrap();
        std::fs::write(b.join(".hidden.rs"), "fn h(){}").unwrap();
        std::fs::create_dir_all(b.join("node_modules/pkg")).unwrap();
        std::fs::write(b.join("node_modules/pkg/dep.js"), "x").unwrap();

        let opts = |skip_hidden| FilterOptions {
            includes: vec![],
            excludes: vec![],
            respect_gitignore: false,
            default_ignore_list: true,
            skip_hidden,
            require_known_kind: false,
        };
        let names = |paths: Vec<std::path::PathBuf>, base: &std::path::Path| {
            let mut v: Vec<String> = paths
                .iter()
                .map(|p| {
                    p.strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/")
                })
                .collect();
            v.sort();
            v
        };

        let got = names(FileFilter::new(opts(true)).walk(b), b);
        assert_eq!(got, vec!["keep.rs"], "skip_hidden=true drops .hidden.rs and node_modules");

        let got2 = names(FileFilter::new(opts(false)).walk(b), b);
        assert!(got2.contains(&".hidden.rs".to_string()), "skip_hidden=false keeps hidden");
        assert!(
            !got2.iter().any(|p| p.starts_with("node_modules/")),
            "node_modules pruned regardless"
        );
    }

    #[test]
    fn walk_subtree_returns_base_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let b = dir.path();
        std::fs::create_dir_all(b.join("prerelease/sub")).unwrap();
        std::fs::write(b.join("prerelease/top.md"), "x").unwrap();
        std::fs::write(b.join("prerelease/sub/deep.md"), "x").unwrap();
        std::fs::write(b.join("other.md"), "x").unwrap(); // outside the subtree

        let f = FileFilter::new(FilterOptions {
            includes: vec![],
            excludes: vec![],
            respect_gitignore: false,
            default_ignore_list: true,
            skip_hidden: true,
            require_known_kind: true,
        });
        let mut got: Vec<String> = f
            .walk_subtree(b, std::path::Path::new("prerelease"))
            .iter()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec!["prerelease/sub/deep.md", "prerelease/top.md"],
            "paths are base-relative and confined to the subtree"
        );
    }

    #[test]
    fn license_files_are_default_skipped_by_stem() {
        let f = FileFilter::new(FilterOptions {
            includes: vec![],
            excludes: vec![],
            respect_gitignore: false,
            default_ignore_list: true,
            skip_hidden: true,
            require_known_kind: false,
        });
        for path in [
            "LICENSE",
            "LICENSE.md",
            "LICENSE-MIT",
            "license.txt",
            "COPYING",
            "COPYING.lesser",
            "NOTICE",
            "NOTICE.md",
            "PATENTS",
            "docs/LICENCE.md",
        ] {
            assert!(!f.allows(path), "{path} should be skipped");
        }
        for path in ["licensed-features.md", "notices.md", "src/licenser.rs"] {
            assert!(f.allows(path), "{path} should be allowed");
        }
    }
}
