//! `mn-skills` — the embedded advanced-search `SKILL.md` plus the harness
//! detection, path-resolution, and idempotent install logic shared by the
//! `mnm skills` CLI noun and the `install_search_skill` MCP tool.

#![doc(html_root_url = "https://docs.rs/mn-skills/0.1.0")]

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
    install, remove, status, HarnessInstall, HarnessRemove, HarnessStatus, InstallAction,
    InstallReport, RemoveAction, RemoveReport, StatusReport,
};

/// The skill's folder name and frontmatter `name` (open Agent Skills standard
/// requires the two to match).
pub const SKILL_NAME: &str = "midnight-advanced-search";

/// The canonical `SKILL.md` body, embedded at build time. This is the single
/// source of truth written into every harness.
#[must_use]
pub const fn skill_markdown() -> &'static str {
    include_str!("../assets/midnight-advanced-search/SKILL.md")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse the `---`-delimited YAML frontmatter at the top of the embedded
    /// `SKILL.md`. Returns the frontmatter block (between the first two `---`
    /// lines).
    fn frontmatter() -> String {
        let md = skill_markdown();
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
    fn frontmatter_is_valid_and_name_matches_folder() {
        let fm: Frontmatter = serde_yaml::from_str(&frontmatter()).expect("frontmatter parses");
        assert_eq!(fm.name, SKILL_NAME, "frontmatter name must equal SKILL_NAME");
        assert!(!fm.description.trim().is_empty(), "description must be non-empty");
        let n = fm.description.chars().count();
        assert!(n <= 1024, "description must be <= 1024 chars (open-standard cap); was {n}");
    }

    #[test]
    fn name_matches_open_standard_regex() {
        // ^[a-z0-9]+(-[a-z0-9]+)*$ — lowercase alnum, single-hyphen separated.
        let ok = SKILL_NAME.split('-').all(|seg| {
            !seg.is_empty()
                && seg
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        });
        assert!(ok, "SKILL_NAME `{SKILL_NAME}` violates the open-standard name regex");
    }
}

#[cfg(test)]
mod catalog_drift_tests {
    //! Half of the registry↔docs drift guard. The other half lives in
    //! `mn-retrieval`'s `facets.rs` (`registry_is_exactly_v1_keys`), which pins
    //! `facets()` to this same 17-key set. If a facet is added/removed in the
    //! registry, that test fails first; update both the registry pin and this
    //! list, and add the row to `filters-and-modes.md`. (We assert against a
    //! literal list rather than depending on `mn-retrieval`, which would drag
    //! `mn-store`/sqlx into this crate's test build.)

    /// The v1 facet wire keys, mirroring `mn_retrieval::facets::facets()`.
    const FACET_KEYS: &[&str] = &[
        "attribution", "content_type", "kind", "source_kind", "source_slug",
        "language", "tags", "heading_path", "symbol", "package", "verified",
        "deprecated", "language_target", "sdk_dependency", "ingested_at",
        "source_modified_at", "token_count",
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
