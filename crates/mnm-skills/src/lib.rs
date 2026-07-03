//! `mnm-skills` — the registry of embedded Agent Skill bundles plus the harness
//! detection, path-resolution, and idempotent install logic shared by the
//! `mnm skills` CLI noun and the `install_skill` MCP tool.
//!
//! The crate is registry-driven: every bundled skill is one [`SkillBundle`]
//! entry in [`SKILLS`], with its assets embedded from `assets/<name>/` at build
//! time. Adding skill N+1 is a new `assets/<name>/` directory plus one registry
//! entry — no install/CLI/MCP logic changes.

#![doc(html_root_url = "https://docs.rs/mnm-skills/0.1.0")]

pub mod detect;
pub mod env;
pub mod error;
pub mod harness;
pub mod install;

pub use detect::{base_dir, detect};
pub use env::{SkillEnv, StdSkillEnv};
pub use error::SkillError;
pub use harness::{Harness, Scope};
pub use install::{
    install, remove, status, status_in, HarnessInstall, HarnessRemove, HarnessStatus,
    InstallAction, InstallReport, RemoveAction, RemoveReport, SkillInstall, SkillRemove,
    SkillStatus, StatusReport,
};

/// One embedded Agent Skill bundle: a folder name plus the files shipped under
/// `assets/<name>/`, each embedded at build time.
///
/// The `name` is the bundle's owned-directory name in every harness AND its
/// frontmatter `name` (the open Agent Skills standard requires the two to
/// match). It is also the value the CLI `--skill` flag and the MCP
/// `install_skill` `skill` enum accept.
#[derive(Debug, Clone, Copy)]
pub struct SkillBundle {
    /// Folder name / frontmatter `name` / wire id.
    pub name: &'static str,
    /// The bundled files as `(relative path, body)`. `SKILL.md` is always entry
    /// 0; the installer ships every entry verbatim into the owned dir.
    pub files: &'static [(&'static str, &'static str)],
}

impl SkillBundle {
    /// The canonical `SKILL.md` body (bundle entry 0). A convenience for the
    /// frontmatter checks and any single-file consumer.
    ///
    /// # Panics
    ///
    /// Every bundle ships `SKILL.md` as entry 0 (the `skill_bundle!` macro
    /// enforces a non-empty file list). A hand-built bundle with an empty
    /// `files` slice trips a `debug_assert!` in debug builds and panics on the
    /// index in release.
    #[must_use]
    pub const fn skill_markdown(&self) -> &'static str {
        debug_assert!(!self.files.is_empty(), "SkillBundle must ship SKILL.md as entry 0");
        self.files[0].1
    }
}

/// Assemble a [`SkillBundle`], embedding every listed file from
/// `assets/<name>/<rel>` at build time (via `include_str!` + `concat!`). This is
/// the one place a new skill's assets are wired in: `skill_bundle!("<name>",
/// ["SKILL.md", "references/…"])`.
macro_rules! skill_bundle {
    ($name:literal, [ $($rel:literal),+ $(,)? ]) => {
        $crate::SkillBundle {
            name: $name,
            files: &[
                $( ($rel, include_str!(concat!("../assets/", $name, "/", $rel))) ),+
            ],
        }
    };
}

/// The registry of every skill bundled into the installer, in install order.
///
/// Adding skill N+1 = a new `assets/<name>/` directory + one `skill_bundle!`
/// entry here. Nothing in `install.rs`, the CLI, or the MCP tool needs editing:
/// they all iterate this slice.
pub const SKILLS: &[SkillBundle] = &[skill_bundle!(
    "midnight-advanced-search",
    [
        "SKILL.md",
        "references/filters-and-modes.md",
        "references/advanced-techniques.md",
        "references/rerank-instructions.md",
    ]
)];

/// The advanced-search skill's name. The MCP low-recall search nudge keys off
/// this specific bundle (`installed(SEARCH_SKILL, env)`), so its behavior is
/// preserved even as other skills are added.
pub const SEARCH_SKILL: &str = "midnight-advanced-search";

/// Look up a bundle by exact `name`.
#[must_use]
pub fn bundle(name: &str) -> Option<&'static SkillBundle> {
    SKILLS.iter().find(|b| b.name == name)
}

/// Every bundled skill name, in registry order. The MCP `skill` enum and the
/// CLI `--skill` help are generated from this.
#[must_use]
pub fn skill_names() -> Vec<&'static str> {
    SKILLS.iter().map(|b| b.name).collect()
}

/// Resolve a `--skill` / `skill` selector to bundles.
///
/// An empty selector means "all bundled skills" — the CLI default and the MCP
/// omit=all semantics — so callers pass the user's explicit names or an empty
/// slice. Names are de-duplicated while preserving first-seen order.
///
/// # Errors
///
/// [`SkillError::UnknownSkill`] for any name not present in [`SKILLS`].
pub fn select(names: &[&str]) -> Result<Vec<&'static SkillBundle>, SkillError> {
    if names.is_empty() {
        return Ok(SKILLS.iter().collect());
    }
    let mut out: Vec<&'static SkillBundle> = Vec::with_capacity(names.len());
    for &name in names {
        let b = bundle(name).ok_or_else(|| SkillError::UnknownSkill {
            name: name.to_owned(),
            known: skill_names().join(", "),
        })?;
        if !out.iter().any(|e| e.name == b.name) {
            out.push(b);
        }
    }
    Ok(out)
}

/// `true` when `skill`'s `SKILL.md` exists for ANY harness at user scope. Used
/// by the MCP search projector's low-result nudge, which passes [`SEARCH_SKILL`].
#[must_use]
pub fn installed(skill: &str, env: &impl SkillEnv) -> bool {
    let Ok(base) = base_dir(Scope::User, env) else {
        return false;
    };
    Harness::ALL
        .into_iter()
        .any(|h| h.skill_file(skill, Scope::User, &base).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse the `---`-delimited YAML frontmatter at the top of `md`, returning
    /// the block between the first two `---` lines.
    fn frontmatter(md: &str) -> String {
        let mut lines = md.lines();
        assert_eq!(lines.next(), Some("---"), "SKILL.md must open with `---`");
        let mut block = String::new();
        for line in lines {
            if line == "---" {
                return block;
            }
            block.push_str(line);
            block.push('\n');
        }
        panic!("SKILL.md frontmatter not closed with `---`");
    }

    #[derive(serde::Deserialize)]
    struct Frontmatter {
        name: String,
        description: String,
    }

    #[test]
    fn registry_is_nonempty_and_search_skill_present() {
        assert!(!SKILLS.is_empty(), "SKILLS must ship at least one bundle");
        assert!(bundle(SEARCH_SKILL).is_some(), "SEARCH_SKILL must name a real registry entry");
    }

    #[test]
    fn select_empty_is_all_dedupes_and_rejects_unknown() {
        // Empty selector == every bundle (CLI default / MCP omit).
        assert_eq!(select(&[]).unwrap().len(), SKILLS.len());
        // Repeats collapse (the dedupe/skip branch).
        assert_eq!(select(&[SEARCH_SKILL, SEARCH_SKILL]).unwrap().len(), 1);
        // Unknown name errors, not silently ignored.
        assert!(matches!(select(&["no-such-skill"]), Err(SkillError::UnknownSkill { .. })));
        // `bundle` mirrors `select`.
        assert!(bundle("no-such-skill").is_none());
        // NOTE: `select` validates every name against the registry, so the
        // "two DISTINCT names, first-seen order" push branch cannot be exercised
        // here while only one skill is registered (a synthetic name would just
        // error as unknown). First-seen ordering of a multi-bundle selection is
        // instead covered at the install layer by
        // `install::tests::multi_bundle_install_and_remove_are_isolated_per_skill`.
    }

    #[test]
    fn every_bundle_frontmatter_is_valid_and_name_matches_folder() {
        for b in SKILLS {
            let fm: Frontmatter = serde_yaml::from_str(&frontmatter(b.skill_markdown()))
                .unwrap_or_else(|e| panic!("{}: frontmatter parses: {e}", b.name));
            assert_eq!(fm.name, b.name, "{}: frontmatter name must equal folder name", b.name);
            assert!(!fm.description.trim().is_empty(), "{}: description must be non-empty", b.name);
            let n = fm.description.chars().count();
            assert!(
                n <= 1024,
                "{}: description must be <= 1024 chars (open-standard cap); was {n}",
                b.name
            );
        }
    }

    #[test]
    fn every_bundle_name_matches_open_standard_regex() {
        // ^[a-z0-9]+(-[a-z0-9]+)*$ — lowercase alnum, single-hyphen separated.
        for b in SKILLS {
            let ok = b.name.split('-').all(|seg| {
                !seg.is_empty()
                    && seg
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            });
            assert!(ok, "skill name `{}` violates the open-standard name regex", b.name);
        }
    }

    #[test]
    fn every_bundle_is_self_contained() {
        // No shipped file may point an installed agent at a path that only
        // exists in this repo. Each bundle must stand alone in the user's harness.
        for b in SKILLS {
            for (path, body) in b.files {
                assert!(
                    !body.contains("docs/cookbook/"),
                    "{}/{path} references a repo-only path (docs/cookbook/)",
                    b.name
                );
                assert!(
                    !body.contains("in the midnight-manual repo"),
                    "{}/{path} points the agent at the repo instead of being self-contained",
                    b.name
                );
            }
            // SKILL.md must link every reference the bundle ships (relative paths).
            let skill = b.skill_markdown();
            for (path, _) in b.files {
                if *path == "SKILL.md" {
                    continue;
                }
                assert!(skill.contains(path), "{}: SKILL.md never links shipped `{path}`", b.name);
            }
        }
    }

    #[test]
    fn every_bundle_manifest_is_complete() {
        for b in SKILLS {
            assert_eq!(b.files[0].0, "SKILL.md", "{}: SKILL.md must be bundle entry 0", b.name);
            let skill = b.skill_markdown();
            for (path, _) in b.files {
                if *path == "SKILL.md" {
                    continue;
                }
                assert!(
                    skill.contains(path),
                    "{}: manifest ships `{path}` but SKILL.md never links it",
                    b.name
                );
            }
        }
    }

    #[test]
    fn every_bundle_file_is_nonempty() {
        for b in SKILLS {
            for (path, body) in b.files {
                assert!(!body.trim().is_empty(), "{}/{path} is empty", b.name);
            }
        }
    }
}

#[cfg(test)]
mod installed_tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Temp-dir fake mirroring the `FakeEnv` the install tests use.
    struct FakeEnv {
        home: PathBuf,
    }
    impl SkillEnv for FakeEnv {
        fn home_dir(&self) -> Option<PathBuf> {
            Some(self.home.clone())
        }
        fn current_dir(&self) -> Option<PathBuf> {
            Some(self.home.clone())
        }
    }

    #[test]
    fn false_on_empty_home() {
        let tmp = TempDir::new().unwrap();
        let env = FakeEnv { home: tmp.path().to_path_buf() };
        assert!(!installed(SEARCH_SKILL, &env));
    }

    #[test]
    fn true_after_writing_skill_file_for_one_harness() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        let file = Harness::ClaudeCode.skill_file(SEARCH_SKILL, Scope::User, &home);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, bundle(SEARCH_SKILL).unwrap().skill_markdown()).unwrap();
        let env = FakeEnv { home };
        assert!(installed(SEARCH_SKILL, &env));
    }

    #[test]
    fn is_per_skill_not_any_skill() {
        // Writing the search skill must not report a different skill as present:
        // the nudge keys off one specific bundle.
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        let file = Harness::ClaudeCode.skill_file(SEARCH_SKILL, Scope::User, &home);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "x").unwrap();
        let env = FakeEnv { home };
        assert!(installed(SEARCH_SKILL, &env));
        assert!(!installed("some-other-skill", &env));
    }
}

#[cfg(test)]
mod catalog_drift_tests {
    //! Half of the registry↔docs drift guard. The other half lives in
    //! `mnm-retrieval`'s `facets.rs` (`registry_is_exactly_v1_keys`), which pins
    //! `facets()` to this same 17-key set. If a facet is added/removed in the
    //! registry, that test fails first; update both the registry pin and this
    //! list, and add the row to `filters-and-modes.md`. (We assert against a
    //! literal list rather than depending on `mnm-retrieval`, which would drag
    //! `mnm-store`/sqlx into this crate's test build.)

    /// The v1 facet wire keys, mirroring `mnm_retrieval::facets::facets()`.
    const FACET_KEYS: &[&str] = &[
        "attribution",
        "content_type",
        "kind",
        "source_kind",
        "source_slug",
        "language",
        "tags",
        "heading_path",
        "symbol",
        "package",
        "verified",
        "deprecated",
        "language_target",
        "sdk_dependency",
        "ingested_at",
        "source_modified_at",
        "token_count",
    ];

    const FILTERS_AND_MODES: &str =
        include_str!("../assets/midnight-advanced-search/references/filters-and-modes.md");

    #[test]
    fn catalog_documents_every_facet_key() {
        for key in FACET_KEYS {
            let needle = format!("`{key}`");
            assert!(
                FILTERS_AND_MODES.contains(&needle),
                "filters-and-modes.md is missing facet `{key}` from its catalog"
            );
        }
    }
}
